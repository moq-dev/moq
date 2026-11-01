//! Tests for [`send::Stream::acked`] and [`send::Stream::poll_acked`], which report whether the
//! peer acknowledged a send stream's prefix

use std::task::Poll;

use super::*;
use crate::connection::waits::Counter;

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

fn poll_acked(
	pair: &mut Pair,
	ch: ConnectionHandle,
	s: StreamId,
	waiter: &kio::Waiter,
	offset: send::Offset,
) -> Poll<Result<(), send::Error>> {
	pair.client_send(ch, s).poll_acked(waiter, offset)
}

/// Transmit the client's pending packets and drop them
fn lose_client_packets(pair: &mut Pair) {
	pair.client.drive(pair.time, pair.server.addr);
	assert!(!pair.client.outbound.is_empty());
	pair.client.outbound.clear();
}

/// An offset is acknowledged only once every byte before it is, and only that wakes its waiter.
#[test]
fn waits_for_contiguous_prefix() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.drive();

	pair.client_send(client_ch, s).write(&mut &b"lost"[..]).unwrap();
	let lost = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);

	// Acknowledged, but behind the lost packet: the middle of the stream, not its prefix
	pair.client_send(client_ch, s).write(&mut &b"after"[..]).unwrap();
	let after = pair.client_send(client_ch, s).offset();
	let waiter = Counter::new();
	assert!(poll_acked(&mut pair, client_ch, s, &waiter, after).is_pending());
	let acks = pair.client_conn_mut(client_ch).stats().frame_rx.acks;
	while pair.client_conn_mut(client_ch).stats().frame_rx.acks == acks {
		assert!(pair.step(), "never acknowledged");
	}
	assert_eq!(waiter.wakes(), 0);
	assert_eq!(acked(&mut pair, client_ch, s, after), Ok(false));

	// The retransmission fills the gap, which acknowledges both at once
	pair.drive();
	assert_eq!(waiter.wakes(), 1);
	assert_eq!(poll_acked(&mut pair, client_ch, s, &waiter, after), Poll::Ready(Ok(())));
	assert_eq!(acked(&mut pair, client_ch, s, lost), Ok(true));
	assert_eq!(acked(&mut pair, client_ch, s, after), Ok(true));
	assert!(pair.client_conn_mut(client_ch).stats().path.lost_packets > 0);
}

/// A waiter past the offset the acknowledged prefix reached wakes with the rest, polls again and
/// registers anew, so it learns of a later ACK too.
#[test]
fn later_waiter_registers_again() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.drive();

	// The first packet is held, and the second lost
	pair.client_send(client_ch, s).write(&mut &b"first"[..]).unwrap();
	let first = pair.client_send(client_ch, s).offset();
	pair.client.drive(pair.time, pair.server.addr);
	pair.client.delay_outbound();
	pair.client_send(client_ch, s).write(&mut &b"second"[..]).unwrap();
	let second = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);
	let (first_waiter, second_waiter) = (Counter::new(), Counter::new());
	assert!(poll_acked(&mut pair, client_ch, s, &second_waiter, second).is_pending());
	assert!(poll_acked(&mut pair, client_ch, s, &first_waiter, first).is_pending());

	// Only the first arrives at first
	pair.client.finish_delay();
	while first_waiter.wakes() == 0 {
		assert!(pair.step(), "never acknowledged");
	}
	assert_eq!(
		poll_acked(&mut pair, client_ch, s, &first_waiter, first),
		Poll::Ready(Ok(()))
	);
	assert_eq!(second_waiter.wakes(), 1);
	assert!(poll_acked(&mut pair, client_ch, s, &second_waiter, second).is_pending());

	// The retransmission wakes the second again
	pair.drive();
	assert_eq!(second_waiter.wakes(), 2);
	assert_eq!(
		poll_acked(&mut pair, client_ch, s, &second_waiter, second),
		Poll::Ready(Ok(()))
	);
}

/// Without a poll waiting on it, an acknowledgment leaves no waiter behind, and a later poll
/// still finds it.
#[test]
fn unpolled_is_not_reported() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	pair.client_send(client_ch, s).write(&mut &b"payload"[..]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	pair.drive();
	assert_eq!(pair.client_conn_mut(client_ch).waiting_streams(), 0);
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

/// A finished stream is freed by the ACK that completes it, which wakes a waiting offset as well
/// as the end.
#[test]
fn finished_stream_reports_before_freed() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);
	pair.drive();

	pair.client_send(client_ch, s).write(&mut &b"payload"[..]).unwrap();
	let offset = pair.client_send(client_ch, s).offset();
	let (acked_waiter, end) = (Counter::new(), Counter::new());
	assert!(poll_acked(&mut pair, client_ch, s, &acked_waiter, offset).is_pending());
	pair.client_send(client_ch, s).finish().unwrap();
	assert!(pair.client_send(client_ch, s).poll_end(&end).is_pending());
	pair.drive();

	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	assert_eq!((acked_waiter.wakes(), end.wakes()), (1, 1));
	assert_eq!(
		poll_acked(&mut pair, client_ch, s, &acked_waiter, offset),
		Poll::Ready(Ok(()))
	);
	assert_eq!(
		pair.client_send(client_ch, s).poll_end(&end),
		Poll::Ready(Ok(send::End::Finished))
	);
	// The freed stream's end is kept until released, so the offset is still answered.
	assert_eq!(acked(&mut pair, client_ch, s, offset), Ok(true));
	assert_eq!(pair.client_send(client_ch, s).end(), Ok(Some(send::End::Finished)));
	pair.client_send(client_ch, s).release();
	assert_eq!(acked(&mut pair, client_ch, s, offset), Err(send::Error::Closed));
	assert_eq!(pair.client_send(client_ch, s).end(), Err(send::Error::Closed));
}

/// Opens a stream whose written prefix and the rest are both lost, resets it after the prefix,
/// and parks a waiter on the prefix, returning the stream, the prefix's end, and the waiter
fn reset_after_prefix(
	pair: &mut Pair,
	client_ch: ConnectionHandle,
	server_ch: ConnectionHandle,
) -> (StreamId, send::Offset, Counter) {
	let s = open(pair, client_ch, server_ch);
	pair.client_send(client_ch, s).write(&mut &b"kept"[..]).unwrap();
	let kept = pair.client_send(client_ch, s).offset();
	let reset = pair.client_send(client_ch, s).offset();
	pair.client_send(client_ch, s).write(&mut &b"dropped"[..]).unwrap();
	let dropped = pair.client_send(client_ch, s).offset();
	lose_client_packets(pair);

	pair.client_send(client_ch, s).reset_at(reset, VarInt(42)).unwrap();
	let waiter = Counter::new();
	assert_eq!(
		poll_acked(pair, client_ch, s, &waiter, dropped),
		Poll::Ready(Err(send::Error::Closed))
	);
	assert!(poll_acked(pair, client_ch, s, &waiter, kept).is_pending());
	(s, kept, waiter)
}

/// A reset fails the offsets past its Reliable Size at once, while a waiter before it still
/// wakes once the prefix arrives.
#[test]
fn reset_fails_past_reliable_size() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let (_, _, waiter) = reset_after_prefix(&mut pair, client_ch, server_ch);

	// The retransmitted prefix is acknowledged by the ACK that frees the stream
	pair.drive();
	assert_eq!(pair.client_streams(client_ch).send_streams(), 0);
	assert_eq!(waiter.wakes(), 1);
}

/// The waiter a reset's acknowledged prefix wakes learns it was acknowledged, even though the
/// same ACK freed the stream.
#[test]
#[ignore = "a stream its reset freed answers Closed for every offset, losing the acknowledged prefix the Acked event carried (quest/m1/quic-api/waits.md)"]
fn reset_keeps_its_acked_prefix() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let (s, kept, waiter) = reset_after_prefix(&mut pair, client_ch, server_ch);

	pair.drive();
	assert_eq!(waiter.wakes(), 1);
	assert_eq!(poll_acked(&mut pair, client_ch, s, &waiter, kept), Poll::Ready(Ok(())));
}

/// A waiter whose offset a later reset drops wakes to learn so, even beside a lower waiter the
/// reset keeps.
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
	let (kept_waiter, dropped_waiter) = (Counter::new(), Counter::new());
	assert!(poll_acked(&mut pair, client_ch, s, &kept_waiter, kept).is_pending());
	assert!(poll_acked(&mut pair, client_ch, s, &dropped_waiter, dropped).is_pending());

	pair.client_send(client_ch, s).reset_at(kept, VarInt(42)).unwrap();
	assert_eq!(pair.client_send(client_ch, s).offset(), kept);
	assert_eq!(dropped_waiter.wakes(), 1);
	assert_eq!(
		poll_acked(&mut pair, client_ch, s, &dropped_waiter, dropped),
		Poll::Ready(Err(send::Error::Closed))
	);
}

/// A peer that stopped reading will not read what it acknowledges, before or after the
/// offset was taken, and a waiter wakes to learn so.
#[test]
fn stop_sending_fails() {
	let _guard = subscribe();
	let mut pair = pair(true);
	let (client_ch, server_ch) = pair.connect();
	let s = open(&mut pair, client_ch, server_ch);

	pair.client_send(client_ch, s).write(&mut &b"unread"[..]).unwrap();
	let before = pair.client_send(client_ch, s).offset();
	lose_client_packets(&mut pair);
	let waiter = Counter::new();
	assert!(poll_acked(&mut pair, client_ch, s, &waiter, before).is_pending());

	pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
	pair.drive();
	assert_eq!(
		pair.client_send(client_ch, s).end(),
		Ok(Some(send::End::Stopped(VarInt(7))))
	);
	let after = pair.client_send(client_ch, s).offset();
	assert_eq!(waiter.wakes(), 1);
	assert_eq!(
		poll_acked(&mut pair, client_ch, s, &waiter, before),
		Poll::Ready(Err(send::Error::Closed))
	);
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
