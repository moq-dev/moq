//! Tests for [`send::Stream::acked`], which reports whether the peer acknowledged a send stream's prefix

use super::*;

const LATENCY: Duration = Duration::from_millis(10);

/// A pair with a one-way delay, whose server does or does not advertise `reset_stream_at`
fn pair(reset_stream_at: bool) -> Pair {
	let mut config = server_config();
	let mut transport = cubic_transport();
	transport.reset_stream_at = reset_stream_at;
	config.transport = transport;
	let mut pair = Pair::new(Default::default(), config);
	pair.latency = LATENCY;
	pair
}

/// Open a client stream that the server has accepted, with nothing acknowledged yet
fn open(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle) -> StreamId {
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"header"[..]).unwrap();
	pair.drive();
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s));
	s
}

fn acked(pair: &mut Pair, ch: ConnectionHandle, s: StreamId, offset: send::Offset) -> Result<bool, send::Error> {
	pair.client_send(ch, s).acked(offset)
}

/// The prefix the client last reported acknowledged on `s`, draining its events
fn reported(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> Option<u64> {
	let mut reported = None;
	while let Some(event) = pair.client_conn_mut(ch).poll() {
		if let Event::Stream(StreamEvent::Acked { id, offset }) = event
			&& id == s
		{
			reported = Some(offset);
		}
	}
	reported
}

/// Transmit the client's pending packets and drop them
fn lose_client_packets(pair: &mut Pair) {
	pair.client.drive(pair.time, pair.server.addr);
	assert!(!pair.client.outbound.is_empty());
	pair.client.outbound.clear();
}

/// An offset is acknowledged only once every byte before it is, and the event that reports it
/// fires only for an offset someone polled.
#[test]
fn waits_for_contiguous_prefix() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.drive();
	reported(&mut pair, client_ch, s);

	pair.client_send(client_ch, s).write(&mut &b"lost"[..]).unwrap();
	let lost = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);

	// Acknowledged, but behind the lost packet: the middle of the stream, not its prefix
	pair.client_send(client_ch, s).write(&mut &b"after"[..]).unwrap();
	let after = pair.client_send(client_ch, s).offset();
	let acks = pair.client_conn_mut(client_ch).stats().frame_rx.acks;
	while pair.client_conn_mut(client_ch).stats().frame_rx.acks == acks {
		assert!(pair.step(), "never acknowledged");
	}
	assert_eq!(acked(&mut pair, client_ch, s, after), Ok(false));

	// The retransmission fills the gap, which acknowledges both at once
	pair.drive();
	assert_eq!(reported(&mut pair, client_ch, s), Some(b"headerlostafter".len() as u64));
	assert_eq!(acked(&mut pair, client_ch, s, lost), Ok(true));
	assert_eq!(acked(&mut pair, client_ch, s, after), Ok(true));
	assert!(pair.client_conn_mut(client_ch).stats().path.lost_packets > 0);
}

/// A waiter past the offset an event reports polls again and registers anew, so it learns of
/// a later ACK too.
#[test]
fn later_waiter_registers_again() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.drive();
	reported(&mut pair, client_ch, s);

	// The first packet is held, and the second lost
	pair.client_send(client_ch, s).write(&mut &b"first"[..]).unwrap();
	let first = pair.client_send(client_ch, s).offset();
	pair.client.drive(pair.time, pair.server.addr);
	pair.client.delay_outbound();
	pair.client_send(client_ch, s).write(&mut &b"second"[..]).unwrap();
	let second = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);
	assert_eq!(acked(&mut pair, client_ch, s, second), Ok(false));
	assert_eq!(acked(&mut pair, client_ch, s, first), Ok(false));

	// Only the first arrives at first
	pair.client.finish_delay();
	while acked(&mut pair, client_ch, s, first) != Ok(true) {
		assert!(pair.step(), "never acknowledged");
	}
	assert_eq!(reported(&mut pair, client_ch, s), Some(u64::from(first)));
	assert_eq!(acked(&mut pair, client_ch, s, second), Ok(false));

	// The retransmission reports the second
	pair.drive();
	assert_eq!(reported(&mut pair, client_ch, s), Some(u64::from(second)));
}

/// Without a poll waiting on it, an acknowledgment is not reported.
#[test]
fn unpolled_is_not_reported() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	pair.client_send(client_ch, s).write(&mut &b"payload"[..]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	pair.drive();
	assert_eq!(reported(&mut pair, client_ch, s), None);
	assert_eq!(acked(&mut pair, client_ch, s, offset), Ok(true));
}

/// An offset taken before anything was written, or once everything was acknowledged, is
/// acknowledged at once.
#[test]
fn already_acked() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, _) = pair.connect();
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	let empty = pair.client_send(client_ch, s).offset();
	assert_eq!(acked(&mut pair, client_ch, s, empty), Ok(true));

	pair.client_send(client_ch, s).write(&mut &b"payload"[..]).unwrap();
	pair.drive();
	let all = pair.client_send(client_ch, s).offset();
	assert_eq!(acked(&mut pair, client_ch, s, all), Ok(true));
}

/// A finished stream is freed by the ACK that completes it, which reports a waiting offset
/// before the stream finishes.
#[test]
fn finished_stream_reports_before_freed() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.drive();
	reported(&mut pair, client_ch, s);

	pair.client_send(client_ch, s).write(&mut &b"payload"[..]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	assert_eq!(acked(&mut pair, client_ch, s, offset), Ok(false));
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();

	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	let mut events = Vec::new();
	while let Some(Event::Stream(event)) = pair.client_conn_mut(client_ch).poll() {
		events.push(event);
	}
	let len = b"headerpayload".len() as u64;
	assert_eq!(
		events,
		[
			StreamEvent::Acked { id: s, offset: len },
			StreamEvent::Finished { id: s }
		]
	);
	// The freed stream's end is kept until released, so the offset is still answered.
	assert_eq!(acked(&mut pair, client_ch, s, offset), Ok(true));
	assert_eq!(pair.client_send(client_ch, s).end(), Ok(Some(send::End::Finished)));
	pair.client_send(client_ch, s).release();
	assert_eq!(acked(&mut pair, client_ch, s, offset), Err(send::Error::Closed));
	assert_eq!(pair.client_send(client_ch, s).end(), Err(send::Error::Closed));
}

/// A reset fails the offsets past its Reliable Size at once, while those before it are
/// still acknowledged once the prefix arrives.
#[test]
fn reset_fails_past_reliable_size() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	pair.client_send(client_ch, s).write(&mut &b"kept"[..]).unwrap();
	let kept = pair.client_send(client_ch, s).offset();
	let reset = pair.client_send(client_ch, s).offset();
	pair.client_send(client_ch, s).write(&mut &b"dropped"[..]).unwrap();
	let dropped = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);
	reported(&mut pair, client_ch, s);

	pair.client_send(client_ch, s).reset_at(reset, VarInt(42)).unwrap();
	assert_eq!(acked(&mut pair, client_ch, s, dropped), Err(send::Error::Closed));
	assert_eq!(acked(&mut pair, client_ch, s, kept), Ok(false));
	assert_eq!(reported(&mut pair, client_ch, s), None);

	// The retransmitted prefix is acknowledged before the reset frees the stream
	pair.drive();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	assert_eq!(reported(&mut pair, client_ch, s), Some(b"headerkept".len() as u64));
}

/// A waiter whose offset a later reset drops learns so from the event, even beside a lower
/// waiter the reset keeps.
#[test]
fn reset_reports_a_dropped_waiter() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	// A lower waiter the reset keeps doesn't hide the one it drops
	pair.client_send(client_ch, s).write(&mut &b"kept"[..]).unwrap();
	let kept = pair.client_send(client_ch, s).offset();
	pair.client_send(client_ch, s).write(&mut &b"dropped"[..]).unwrap();
	let dropped = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);
	reported(&mut pair, client_ch, s);
	assert_eq!(acked(&mut pair, client_ch, s, kept), Ok(false));
	assert_eq!(acked(&mut pair, client_ch, s, dropped), Ok(false));

	pair.client_send(client_ch, s).reset_at(kept, VarInt(42)).unwrap();
	assert_eq!(pair.client_send(client_ch, s).offset(), kept);
	assert!(reported(&mut pair, client_ch, s).is_some());
	assert_eq!(acked(&mut pair, client_ch, s, dropped), Err(send::Error::Closed));
}

/// A peer that stopped reading will not read what it acknowledges, before or after the
/// offset was taken, and a waiter learns so from the event.
#[test]
fn stop_sending_fails() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	pair.client_send(client_ch, s).write(&mut &b"unread"[..]).unwrap();
	let before = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);
	assert_eq!(acked(&mut pair, client_ch, s, before), Ok(false));

	pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
	pair.drive();
	assert_eq!(
		pair.client_send(client_ch, s).end(),
		Ok(Some(send::End::Stopped(VarInt(7))))
	);
	let after = pair.client_send(client_ch, s).offset();
	assert_eq!(reported(&mut pair, client_ch, s), Some(b"header".len() as u64));
	for offset in [before, after] {
		assert_eq!(acked(&mut pair, client_ch, s, offset), Err(send::Error::Closed));
	}
}

/// Nothing is acknowledged on a closed connection.
#[test]
fn close_fails() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	pair.client_send(client_ch, s).write(&mut &b"unacked"[..]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);

	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), String::new());
	assert_eq!(acked(&mut pair, client_ch, s, offset), Err(send::Error::Closed));
}

/// An offset from another stream is a caller bug, the one misuse its type leaves possible.
#[test]
#[should_panic(expected = "offset used on another stream")]
fn foreign_offset_panics() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	let other = open(&mut pair, client_ch, server_ch);
	let foreign = pair.client_send(client_ch, other).offset();
	let _ = pair.client_send(client_ch, s).reset_at(foreign, VarInt(42));
}
