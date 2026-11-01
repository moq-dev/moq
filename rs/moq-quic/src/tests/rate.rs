//! Tests for the bitrate caps [`Limits::receive_rate`] and [`Limits::send_rate`] hold a
//! connection to, on the simulated clock

use std::num::NonZeroU64;

use super::*;
use crate::{Limits, Rate};

const LATENCY: Duration = Duration::from_millis(10);
/// 1 Mbps, so the burst a cap allows is this many bytes
const RATE: u64 = 125_000;
/// What the server grants before it applies a cap, as a relay does before auth
const PRE_AUTH: u32 = 16 * 1024;
/// The window the server opens to once it applies a cap: far past a burst, so the rate binds
const WINDOW: u64 = 4 * 1024 * 1024;
/// A datagram the tests send, and the most the send cap may overshoot by, one segment
const DATAGRAM: usize = 1000;

fn pair() -> Pair {
	let mut server = server_config();
	server.transport.limits.receive_window = NonZeroU64::new(PRE_AUTH.into());
	let mut pair = Pair::new(Default::default(), server);
	pair.latency = LATENCY;
	pair
}

fn capped(rate: u64) -> Option<Rate> {
	Some(Rate::BytesPerSecond(NonZeroU64::new(rate).unwrap()))
}

/// The limits a relay applies once auth caps the peer's sending at `rate`
fn cap_receive(rate: u64) -> Limits {
	Limits {
		receive_window: NonZeroU64::new(WINDOW),
		receive_rate: capped(rate),
		..Limits::default()
	}
}

/// Drive the pair for `duration`, calling `app` at least every millisecond of simulated time
fn run(pair: &mut Pair, duration: Duration, mut app: impl FnMut(&mut Pair)) {
	let end = pair.time + duration;
	while pair.time < end {
		app(pair);
		pair.drive_client();
		pair.drive_server();
		let next = [pair.client.next_wakeup(), pair.server.next_wakeup()]
			.into_iter()
			.flatten()
			.chain([pair.time + Duration::from_millis(1), end])
			.min()
			.unwrap();
		pair.time = pair.time.max(next);
	}
}

/// Write `stream` until flow control blocks it
fn write_flat_out(send: &mut send::Stream<'_>) {
	let buf = [0xAB; 16 * 1024];
	while let Ok(n) = send.write(&mut &buf[..]) {
		if n == 0 {
			break;
		}
	}
}

/// Read everything that arrived on `stream`, returning how many bytes
fn read_all(mut recv: recv::Stream<'_>) -> u64 {
	let Ok(mut chunks) = recv.read() else {
		return 0;
	};
	let mut read = 0;
	while let Ok(Some(chunk)) = chunks.next(usize::MAX) {
		read += chunk.bytes.len() as u64;
	}
	let _ = chunks.finalize();
	read
}

/// Bytes the cap allows over `elapsed` after a full bucket, plus any credit granted before it
fn allowance(rate: u64, elapsed: Duration, outstanding: u64) -> u64 {
	let earned = (u128::from(rate) * elapsed.as_nanos() / 1_000_000_000) as u64;
	outstanding + rate + earned
}

/// Open a uni stream from the client the server has accepted
fn open(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle) -> StreamId {
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"x"[..]).unwrap();
	pair.drive();
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s));
	assert_eq!(read_all(pair.server_recv(server_ch, s)), 1);
	s
}

/// A client writing flat out is held to the cap, plus the burst and what it held before.
#[test]
fn a_flat_out_sender_is_held_to_the_cap() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	let outstanding = pair.server_conn_mut(server_ch).stats().credit.available;
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();
	let grants = pair.server_conn_mut(server_ch).stats().frame_tx.max_data;
	let duration = Duration::from_secs(5);
	let mut read = 0;
	run(&mut pair, duration, |pair| {
		write_flat_out(&mut pair.client_send(client_ch, s));
		read += read_all(pair.server_recv(server_ch, s));
	});

	// Granted in eighths of the burst, not a few bytes per packet the peer sends.
	let grants = pair.server_conn_mut(server_ch).stats().frame_tx.max_data - grants;
	assert!(grants <= 9 * duration.as_secs(), "{grants} MAX_DATA frames");

	assert!(
		read <= allowance(RATE, duration, outstanding),
		"read {read} past the cap"
	);
	// Credit arrives a round trip after it is earned, so allow that much shortfall.
	let floor = RATE * duration.as_secs() * 9 / 10;
	assert!(read >= floor, "read {read}, under {floor}: the cap starved the sender");
}

/// A capped peer that sends no stream data is held to the cap by its datagrams, the excess
/// dropped as if lost.
#[test]
fn a_datagram_flood_is_held_to_the_cap() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();

	let duration = Duration::from_secs(5);
	let (mut sent, mut received) = (0, 0);
	run(&mut pair, duration, |pair| {
		// Ten times the cap.
		for _ in 0..(10 * RATE / 1000 / DATAGRAM as u64) {
			if pair
				.client_datagrams(client_ch)
				.send(Bytes::from(vec![0; DATAGRAM]), Default::default())
				.is_ok()
			{
				sent += DATAGRAM as u64;
			}
		}
		while let Some(datagram) = pair.server_datagrams(server_ch).recv() {
			received += datagram.len() as u64;
		}
	});

	assert!(sent > 5 * received, "the flood never exceeded the cap");
	assert!(
		received <= allowance(RATE, duration, 0),
		"received {received} past the cap"
	);
	assert!(received >= RATE * duration.as_secs() * 9 / 10, "received {received}");
}

/// Stream data and datagrams under the cap together lose nothing, though unspent stream
/// credit and datagrams draw on the same allowance.
#[test]
fn mixed_traffic_under_the_cap_loses_no_datagrams() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();

	// Each 40% of the cap, sent every 10ms.
	let per_tick = (RATE * 4 / 10 / 100) as usize;
	let (mut next, mut sent, mut received, mut read) = (pair.time, 0, 0, 0);
	run(&mut pair, Duration::from_secs(5), |pair| {
		if pair.time >= next {
			next += Duration::from_millis(10);
			pair.client_send(client_ch, s)
				.write(&mut &vec![0; per_tick][..])
				.unwrap();
			pair.client_datagrams(client_ch)
				.send(Bytes::from(vec![0; per_tick]), Default::default())
				.unwrap();
			sent += 1;
		}
		while pair.server_datagrams(server_ch).recv().is_some() {
			received += 1;
		}
		read += read_all(pair.server_recv(server_ch, s));
	});
	pair.drive();
	while pair.server_datagrams(server_ch).recv().is_some() {
		received += 1;
	}
	read += read_all(pair.server_recv(server_ch, s));

	assert_eq!(received, sent, "a datagram under the cap was dropped");
	assert_eq!(read, sent * per_tick as u64, "stream data under the cap was held back");
}

/// Credit granted before the cap, as before auth, or under a higher cap that is then lowered,
/// is spent at most once more than the new cap allows.
#[test]
fn credit_outstanding_when_the_cap_drops_is_the_only_overshoot() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	// A high cap with the client idle grants it a whole burst it has not spent.
	pair.server_conn_mut(server_ch)
		.set_limits(cap_receive(10 * RATE))
		.unwrap();
	pair.drive();
	let outstanding = pair.server_conn_mut(server_ch).stats().credit.available;
	assert!(outstanding > 9 * RATE, "the high cap granted only {outstanding}");

	pair.server_conn_mut(server_ch)
		.set_limits(Limits {
			receive_rate: capped(RATE),
			..Limits::default()
		})
		.unwrap();
	let duration = Duration::from_secs(5);
	let mut read = 0;
	run(&mut pair, duration, |pair| {
		write_flat_out(&mut pair.client_send(client_ch, s));
		read += read_all(pair.server_recv(server_ch, s));
	});

	assert!(read >= outstanding, "the client never spent the credit it held");
	assert!(
		read <= allowance(RATE, duration, outstanding),
		"read {read} past the cap and the credit outstanding at the change"
	);
}

/// An idle peer never holds more than one burst of credit, however long it waits.
#[test]
fn an_idle_sender_banks_at_most_one_burst() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();

	// Spend the burst, then idle long enough to earn many more.
	run(&mut pair, Duration::from_secs(1), |pair| {
		write_flat_out(&mut pair.client_send(client_ch, s));
		read_all(pair.server_recv(server_ch, s));
	});
	run(&mut pair, Duration::from_secs(30), |pair| {
		read_all(pair.server_recv(server_ch, s));
	});

	let available = pair.server_conn_mut(server_ch).stats().credit.available;
	assert!(available <= RATE, "an idle peer holds {available} bytes of credit");
	assert!(
		available >= RATE * 7 / 8,
		"an idle peer holds only {available} bytes of credit"
	);
}

/// Stopping a stream returns its memory but not the rate it spent.
#[test]
fn cancellation_refunds_no_rate() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();

	// Spend the burst without reading it, then discard it.
	run(&mut pair, Duration::from_millis(100), |pair| {
		write_flat_out(&mut pair.client_send(client_ch, s));
	});
	let held = pair.server_conn_mut(server_ch).stats().credit.available;
	assert!(held < RATE / 8, "the burst was not spent: {held} bytes of credit left");
	pair.server_recv(server_ch, s).stop(0u32.into()).unwrap();
	pair.drive_server();

	let available = pair.server_conn_mut(server_ch).stats().credit.available;
	assert!(available < RATE / 8, "the stop refunded {available} bytes of credit");
}

/// A frame larger than one rate interval completes within the memory window, and pacing alone
/// never evicts the lease waiting on it.
#[test]
fn a_frame_larger_than_the_burst_arrives_gradually() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();

	let frame = 3 * RATE as usize;
	let data = vec![0xCD; frame];
	let mut written = 0;
	let mut leased = None;
	let start = pair.time;
	run(&mut pair, Duration::from_secs(5), |pair| {
		if written < frame
			&& let Ok(n) = pair.client_send(client_ch, s).write(&mut &data[written..])
		{
			written += n;
		}
		if leased.is_none() && pair.server_recv(server_ch, s).lease(frame).0.is_ok() {
			leased = Some(pair.time);
		}
	});

	let leased = leased.expect("the frame never completed") - start;
	assert!(
		leased >= Duration::from_secs(1),
		"the frame arrived in {leased:?}, past the cap"
	);
	assert!(leased <= Duration::from_secs(3), "the frame took {leased:?}");
	assert_eq!(pair.server_conn_mut(server_ch).stats().credit.evicted, 0);
}

/// Without a cap, the server opens the normal window at once.
#[test]
fn an_uncapped_session_gets_its_window() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch)
		.set_limits(Limits {
			receive_window: NonZeroU64::new(WINDOW),
			receive_rate: Some(Rate::Unlimited),
			..Limits::default()
		})
		.unwrap();
	assert_eq!(pair.server_conn_mut(server_ch).stats().credit.available, WINDOW);
	pair.drive();
	let written = pair
		.client_send(client_ch, s)
		.write(&mut &vec![0; PRE_AUTH as usize * 4][..])
		.unwrap();
	assert_eq!(
		written,
		PRE_AUTH as usize * 4,
		"the client still sees the pre-auth window"
	);
}

/// Raising, lowering, and lifting the cap on a live connection each take effect.
#[test]
fn a_live_cap_change_takes_effect() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();
	// A path well above every cap, so the lifted phase measures the path, not the simulator.
	pair.to_server.bottleneck = Some(Bottleneck::new(20 * RATE, 256 * 1024, false));

	let phase = |pair: &mut Pair, rate: Option<Rate>| {
		if let Some(rate) = rate {
			pair.server_conn_mut(server_ch)
				.set_limits(Limits {
					receive_rate: Some(rate),
					..Limits::default()
				})
				.unwrap();
		}
		let mut read = 0;
		run(pair, Duration::from_secs(4), |pair| {
			write_flat_out(&mut pair.client_send(client_ch, s));
			read += read_all(pair.server_recv(server_ch, s));
		});
		read
	};

	let base = phase(&mut pair, None);
	let raised = phase(&mut pair, capped(4 * RATE));
	let lowered = phase(&mut pair, capped(RATE));
	let lifted = phase(&mut pair, Some(Rate::Unlimited));
	assert!(raised > 3 * base, "raising the cap: {base} then {raised}");
	assert!(lowered < raised / 3, "lowering the cap: {raised} then {lowered}");
	// The outstanding credit of the raised cap, a burst at most, may be spent after the drop.
	assert!(lowered <= allowance(RATE, Duration::from_secs(4), 4 * RATE));
	assert!(lifted > 4 * raised, "lifting the cap: {lowered} then {lifted}");
}

/// A capped sender never sends past the cap, whatever its controller allows, and resumes once
/// it is lifted.
#[test]
fn egress_is_held_to_the_send_cap() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	pair.server_conn_mut(server_ch)
		.set_limits(Limits {
			send_rate: capped(RATE),
			..Limits::default()
		})
		.unwrap();
	let s = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	let sent = |pair: &mut Pair| pair.server_conn_mut(server_ch).stats().udp_tx.bytes;
	let duration = Duration::from_secs(5);
	let before = sent(&mut pair);
	let mut read = 0;
	run(&mut pair, duration, |pair| {
		write_flat_out(&mut pair.server_send(server_ch, s));
		read += read_all(pair.client_recv(client_ch, s));
	});

	let sent = sent(&mut pair) - before;
	assert!(
		sent <= allowance(RATE, duration, DEFAULT_MTU as u64),
		"sent {sent} past the cap"
	);
	assert!(read >= RATE * duration.as_secs() * 8 / 10, "read only {read}");
}

/// Caps set at construction hold both directions from the first byte, the initial window
/// counting as credit already granted.
#[test]
fn caps_set_at_construction_hold() {
	let mut server = server_config();
	server.transport.limits.receive_window = NonZeroU64::new(PRE_AUTH.into());
	server.transport.limits.receive_rate = capped(RATE);
	server.transport.limits.send_rate = capped(RATE);
	let mut pair = Pair::new(Default::default(), server);
	pair.latency = LATENCY;
	let (client_ch, server_ch) = pair.connect();
	let up = open(&mut pair, client_ch, server_ch);
	let down = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();

	let sent = |pair: &mut Pair| pair.server_conn_mut(server_ch).stats().udp_tx.bytes;
	let before = sent(&mut pair);
	let duration = Duration::from_secs(5);
	let (mut up_read, mut down_read) = (0, 0);
	run(&mut pair, duration, |pair| {
		write_flat_out(&mut pair.client_send(client_ch, up));
		write_flat_out(&mut pair.server_send(server_ch, down));
		up_read += read_all(pair.server_recv(server_ch, up));
		down_read += read_all(pair.client_recv(client_ch, down));
	});

	let limit = allowance(RATE, duration, PRE_AUTH.into());
	assert!(up_read <= limit, "read {up_read} past the receive cap");
	assert!(up_read >= RATE * duration.as_secs() * 8 / 10, "read only {up_read}");
	let sent = sent(&mut pair) - before;
	assert!(
		sent <= allowance(RATE, duration, DEFAULT_MTU as u64),
		"sent {sent} past the send cap"
	);
	assert!(down_read >= RATE * duration.as_secs() * 7 / 10, "read only {down_read}");
}

/// A packet that arrives once the credit is due, before the timeout runs, grants that credit
/// rather than retiring the deadline that would have.
#[test]
fn a_packet_past_the_credit_deadline_grants_it() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch).set_limits(cap_receive(RATE)).unwrap();
	// The server never reads, so only the cap grants credit.
	run(&mut pair, Duration::from_secs(1), |pair| {
		write_flat_out(&mut pair.client_send(client_ch, s));
	});

	let deadline = pair
		.server_conn_mut(server_ch)
		.receive_rate_deadline()
		.expect("the sender spent its credit");
	let before = pair.server_conn_mut(server_ch).stats().credit.available;
	// A ping that lands past the deadline and is handled before the timer, as a busy event loop
	// may, and then the late timer.
	pair.time = deadline;
	pair.client_conn_mut(client_ch).ping();
	pair.drive_client();
	pair.time = deadline + LATENCY;
	pair.server.timeouts.remove(&server_ch);
	pair.drive_server();
	pair.server.timeouts.insert(server_ch, deadline);
	pair.drive_server();

	let after = pair.server_conn_mut(server_ch).stats().credit.available;
	assert!(after > before, "credit stayed at {before}: the due grant was lost");
}

/// A peer whose acknowledgments never arrive draws loss probes, which the send cap holds too.
#[test]
fn loss_probes_are_held_to_the_send_cap() {
	/// Low enough that probes past the cap would show.
	const SLOW: u64 = 2_000;
	let mut pair = pair();
	let (_, server_ch) = pair.connect();
	pair.server_conn_mut(server_ch)
		.set_limits(Limits {
			send_rate: capped(SLOW),
			..Limits::default()
		})
		.unwrap();
	let s = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	write_flat_out(&mut pair.server_send(server_ch, s));
	pair.to_server.loss = 1.0;

	let sent = |pair: &mut Pair| pair.server_conn_mut(server_ch).stats().udp_tx.bytes;
	// While the probes back off, before the cap's later sends would repay what they overdrew.
	let duration = Duration::from_secs(3);
	let before = sent(&mut pair);
	run(&mut pair, duration, |_| {});

	let sent = sent(&mut pair) - before;
	assert!(
		sent <= allowance(SLOW, duration, DEFAULT_MTU as u64),
		"sent {sent} past the cap"
	);
}

/// A cap too low to batch its grants sends no `MAX_DATA` without new credit, so a packet
/// arriving on an idle connection cannot start a `MAX_DATA` and ACK ping-pong.
#[test]
fn a_tiny_cap_sends_no_empty_grants() {
	let mut pair = pair();
	let (client_ch, server_ch) = pair.connect();
	open(&mut pair, client_ch, server_ch);
	pair.server_conn_mut(server_ch).set_limits(cap_receive(1)).unwrap();
	assert!(!pair.drive_bounded(), "the connection never went idle");

	let grants = pair.server_conn_mut(server_ch).stats().frame_tx.max_data;
	pair.client_conn_mut(client_ch).ping();
	assert!(!pair.drive_bounded(), "the connection never went idle");
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_tx.max_data, grants);
}
