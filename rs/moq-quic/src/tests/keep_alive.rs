//! A per-connection keep-alive: probes a quiet peer, reports it stale, and closes on a dead one

use super::*;
use crate::connection::{KeepAlive, KeepAliveError};
use crate::entropy;
use crate::sim::{Bottleneck, Link};

const SOFT: Duration = Duration::from_secs(2);
const HARD: Duration = Duration::from_secs(10);

/// A path whose rate collapses to nothing: its queue holds one packet and drops the rest, as a
/// bufferbloated link does once its queue fills
fn collapsed() -> Link {
	let mut link = Link::default();
	link.bottleneck = Some(Bottleneck::timed(1, Duration::from_millis(200)));
	link
}

/// Step `pair` until `until`, calling `each` after every step with the time the step handled,
/// since a step ends by advancing to the next wakeup
fn run(pair: &mut Pair, until: Instant, mut each: impl FnMut(&mut Pair, Instant)) {
	while pair.time < until {
		let at = pair.time;
		if !pair.step() {
			pair.time = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup()).map_or(until, |at| at.min(until));
		}
		each(pair, at);
	}
}

/// A client running the keep-alive against a server with the default 30 s idle timeout
fn connect() -> (Pair, ConnectionHandle, ConnectionHandle) {
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	pair.client_conn_mut(client_ch)
		.set_keep_alive(KeepAlive::new(SOFT, HARD))
		.unwrap();
	(pair, client_ch, server_ch)
}

fn pings(pair: &mut Pair, ch: ConnectionHandle) -> u64 {
	pair.client_conn_mut(ch).stats().frame_tx.ping
}

/// An idle session pings about once per half of `soft` and stays live; a blackholed one turns
/// stale within `soft`, and live again once the path heals before `hard`
#[test]
fn stale_within_soft_and_live_once_healed() {
	let _guard = subscribe();
	let (mut pair, client_ch, _) = connect();

	let before = pings(&mut pair, client_ch);
	let end = pair.time.saturating_add(Duration::from_secs(20));
	run(&mut pair, end, |pair, _| {
		assert!(!pair.client_conn_mut(client_ch).is_stale())
	});
	let idle = pings(&mut pair, client_ch) - before;
	assert!((15..=21).contains(&idle), "{idle} pings in 20 s");

	pair.to_server = collapsed();
	pair.to_client = collapsed();
	let blackholed = pair.time;
	let woken = Counter::new();
	assert!(pair.client_conn_mut(client_ch).poll_stale(&woken, false).is_pending());
	let mut stale = None;
	run(
		&mut pair,
		blackholed.saturating_add(Duration::from_secs(5)),
		|pair, at| {
			if stale.is_none() && pair.client_conn_mut(client_ch).is_stale() {
				stale = Some(at);
			}
		},
	);
	let after = stale.expect("turned stale") - blackholed;
	assert!(after > SOFT / 2 && after <= SOFT, "stale {after:?} after the blackhole");
	assert_eq!(woken.wakes(), 1);
	assert!(!pair.client_conn_mut(client_ch).is_closed());

	pair.to_server = Link::default();
	pair.to_client = Link::default();
	let healed = pair.time;
	let mut live = None;
	run(&mut pair, healed.saturating_add(SOFT), |pair, at| {
		if live.is_none() && !pair.client_conn_mut(client_ch).is_stale() {
			live = Some(at);
		}
	});
	let after = live.expect("live again") - healed;
	assert!(
		after <= SOFT / 2 + Duration::from_millis(100),
		"live {after:?} after healing"
	);
	assert!(!pair.client_conn_mut(client_ch).is_closed());
}

/// A path that stays dead closes the connection as timed out once `hard` passes unheard, well
/// before the 30 s idle timeout
#[test]
fn dead_path_closes_at_hard() {
	let _guard = subscribe();
	let (mut pair, client_ch, _) = connect();
	pair.drive();

	pair.to_server = collapsed();
	pair.to_client = collapsed();
	let blackholed = pair.time;
	let mut closed = None;
	run(
		&mut pair,
		blackholed.saturating_add(Duration::from_secs(20)),
		|pair, at| {
			if closed.is_none() && pair.client_conn_mut(client_ch).is_closed() {
				closed = Some(at);
			}
		},
	);
	let after = closed.expect("closed") - blackholed;
	assert!(
		after > HARD - SOFT && after <= HARD,
		"closed {after:?} after the blackhole"
	);
	let conn = pair.client_conn_mut(client_ch);
	assert_matches!(
		conn.poll_closed(&kio::Waiter::noop()),
		Poll::Ready(ConnectionError::TimedOut)
	);
}

/// The PINGs a client sends while busy sending for `3 * SOFT` over a 1 Mbps bottleneck, every
/// one of them some other mechanism's (such as MTU discovery's) unless a keep-alive adds its own
fn busy_pings(keep_alive: bool) -> u64 {
	entropy::reseed(7);
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	if keep_alive {
		pair.client_conn_mut(client_ch)
			.set_keep_alive(KeepAlive::new(SOFT, HARD))
			.unwrap();
	}
	pair.drive();
	pair.latency = Duration::from_millis(10);
	for link in [&mut pair.to_server, &mut pair.to_client] {
		link.bottleneck = Some(Bottleneck::timed(125_000, Duration::from_millis(50)));
	}
	let before = pings(&mut pair, client_ch);
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let end = pair.time.saturating_add(3 * SOFT);
	let chunk = [0u8; 1000];
	let mut accepted = None;
	while pair.time < end {
		let _ = pair.client_send(client_ch, s).write(&mut &chunk[..]);
		if accepted.is_none() {
			accepted = pair.server_streams(server_ch).accept(Dir::Uni);
		}
		if let Some(id) = accepted {
			let mut recv = pair.server_recv(server_ch, id);
			let mut chunks = recv.read().unwrap();
			while let Poll::Ready(Ok(Some(_))) = chunks.try_next(usize::MAX) {}
			chunks.finalize();
		}
		assert!(!pair.client_conn_mut(client_ch).is_stale());
		if !pair.step() {
			pair.time = min_opt(pair.client.next_wakeup(), pair.server.next_wakeup()).map_or(end, |at| at.min(end));
		}
	}
	assert!(accepted.is_some());
	pings(&mut pair, client_ch) - before
}

/// A session busy sending hears the peer's ACKs, so the keep-alive adds no PING
#[test]
fn busy_session_pays_nothing() {
	let _guard = subscribe();
	assert_eq!(busy_pings(true), busy_pings(false));
}

/// How long after healing an `outage` a client blackholed mid-send hears its peer again (`SOFT` if
/// it never does): the dead path fills its congestion window with packets nothing acknowledges,
/// and its PTO probes back off further apart than `soft`
fn full_window_heal(outage: Duration) -> Duration {
	let (mut pair, client_ch, server_ch) = connect();
	pair.drive();
	pair.latency = Duration::from_millis(10);
	let throttled = || {
		let mut link = Link::default();
		link.bottleneck = Some(Bottleneck::timed(125_000, Duration::from_millis(50)));
		link
	};
	pair.to_server = throttled();
	pair.to_client = throttled();
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let chunk = [0u8; 1000];
	let mut accepted = None;
	// Keep the stream backlogged throughout, so the window stays full while the path is dead.
	let mut send = |pair: &mut Pair| {
		let _ = pair.client_send(client_ch, s).try_write(&mut &chunk[..]);
		if accepted.is_none() {
			accepted = pair.server_streams(server_ch).accept(Dir::Uni);
		}
		if let Some(id) = accepted {
			let mut recv = pair.server_recv(server_ch, id);
			let mut chunks = recv.read().unwrap();
			while let Poll::Ready(Ok(Some(_))) = chunks.try_next(usize::MAX) {}
			chunks.finalize();
		}
	};
	let busy = pair.time.saturating_add(SOFT);
	run(&mut pair, busy, |pair, _| send(pair));

	pair.to_server = collapsed();
	pair.to_client = collapsed();
	let blackholed = pair.time;
	run(&mut pair, blackholed.saturating_add(outage), |pair, _| send(pair));
	assert!(pair.client_conn_mut(client_ch).is_stale());

	pair.to_server = throttled();
	pair.to_client = throttled();
	let healed = pair.time;
	let mut live = None;
	run(&mut pair, healed.saturating_add(SOFT), |pair, at| {
		send(pair);
		if live.is_none() && !pair.client_conn_mut(client_ch).is_stale() {
			live = Some(at);
		}
	});
	assert!(!pair.client_conn_mut(client_ch).is_closed());
	live.map_or(SOFT, |live| live - healed)
}

/// The keep-alive's probe leaves with a full window, so a busy session turns live within half a
/// `soft` of healing instead of waiting for the next backed-off PTO, perhaps past `hard`
#[test]
fn full_window_still_probes_after_healing() {
	let _guard = subscribe();
	// Outages from just past `soft` to just short of `hard`, landing between backed-off PTOs.
	for ms in (3_000..9_000).step_by(500) {
		let after = full_window_heal(Duration::from_millis(ms));
		assert!(
			after <= SOFT / 2 + Duration::from_millis(100),
			"live {after:?} after healing a {ms} ms outage"
		);
	}
}

/// A `hard` past the negotiated idle timeout would never be reached, so it is refused
#[test]
fn refuses_hard_past_the_idle_timeout() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, _) = pair.connect();
	let conn = pair.client_conn_mut(client_ch);
	assert_matches!(
		conn.set_keep_alive(KeepAlive::new(SOFT, Duration::from_secs(31))),
		Err(KeepAliveError::IdleTimeout { .. })
	);
	assert!(!conn.is_stale());
}
