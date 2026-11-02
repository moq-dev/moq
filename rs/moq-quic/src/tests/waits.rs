//! Tests for the `poll_*` waits, counting the wakes of each waiter rather than matching events

use std::task::Poll;

use super::*;
use crate::connection::waits::Counter;

fn pair_with(limits: Limits) -> Pair {
	let server = server::Config {
		transport: transport::Config {
			limits,
			..transport::Config::default()
		},
		..server_config()
	};
	Pair::new(Default::default(), server)
}

/// Open a client stream carrying `data` and accept it on the server
fn open_uni(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle, data: &[u8]) -> StreamId {
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &data[..]).unwrap();
	pair.drive();
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s));
	s
}

/// Poll the server's next chunk of `s`
fn poll_read(
	pair: &mut Pair,
	ch: ConnectionHandle,
	s: StreamId,
	waiter: &kio::Waiter,
) -> Poll<Result<Option<Bytes>, recv::Error>> {
	let mut recv = pair.server_recv(ch, s);
	let mut chunks = recv.read().unwrap();
	let poll = chunks
		.poll_next(waiter, usize::MAX)
		.map(|r| r.map(|chunk| chunk.map(|c| c.bytes)));
	let _ = chunks.finalize();
	poll
}

#[test]
fn read_wakes_every_waiter() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let (first, second) = (Counter::new(), Counter::new());
	assert_matches!(poll_read(&mut pair, server_ch, s, &first), Poll::Ready(Ok(Some(data))) if data == "a");
	assert!(poll_read(&mut pair, server_ch, s, &first).is_pending());
	assert!(poll_read(&mut pair, server_ch, s, &second).is_pending());
	// Registering again while still parked adds nothing.
	assert!(poll_read(&mut pair, server_ch, s, &first).is_pending());

	pair.client_send(client_ch, s).write(&mut &b"b"[..]).unwrap();
	pair.drive();
	assert_eq!((first.wakes(), second.wakes()), (1, 1));
	assert_matches!(poll_read(&mut pair, server_ch, s, &first), Poll::Ready(Ok(Some(data))) if data == "b");
}

#[test]
fn dropped_waiter_leaves_no_registration() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let kept = Counter::new();
	assert!(poll_read(&mut pair, server_ch, s, &kept).is_ready());
	let dropped = Counter::new();
	assert!(poll_read(&mut pair, server_ch, s, &dropped).is_pending());
	let count = dropped.count();
	drop(dropped);
	assert_eq!(pair.server_conn_mut(server_ch).waiting_streams(), 1);

	// The wake finds no live waiter, and the stream's entry goes with it.
	pair.client_send(client_ch, s).write(&mut &b"b"[..]).unwrap();
	pair.drive();
	assert_eq!(count(), 0);
	assert_eq!(pair.server_conn_mut(server_ch).waiting_streams(), 0);
}

#[test]
fn freed_stream_forgets_its_waiters() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let waiter = Counter::new();
	assert!(poll_read(&mut pair, server_ch, s, &waiter).is_ready());
	assert!(poll_read(&mut pair, server_ch, s, &waiter).is_pending());
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();
	assert_eq!(waiter.wakes(), 1);
	assert_matches!(poll_read(&mut pair, server_ch, s, &waiter), Poll::Ready(Ok(None)));
	assert_eq!(pair.server_conn_mut(server_ch).waiting_streams(), 0);
}

#[test]
fn accept_waits_for_the_peer() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let (uni, bi) = (Counter::new(), Counter::new());
	assert!(pair.server_streams(server_ch).poll_accept(&uni, Dir::Uni).is_pending());
	assert!(pair.server_streams(server_ch).poll_accept(&bi, Dir::Bi).is_pending());

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"a"[..]).unwrap();
	pair.drive();
	assert_eq!((uni.wakes(), bi.wakes()), (1, 0), "only the direction opened wakes");
	assert_eq!(
		pair.server_streams(server_ch).poll_accept(&uni, Dir::Uni),
		Poll::Ready(Ok(s))
	);
}

#[test]
fn open_waits_for_stream_credit() {
	let _guard = subscribe();
	let mut pair = pair_with(Limits {
		max_streams: NonZeroU64::new(1),
		..Limits::default()
	});
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let waiter = Counter::new();
	let poll = pair
		.client_streams(client_ch)
		.poll_open(&waiter, Dir::Uni, Default::default());
	assert!(poll.is_pending());

	// The server reading the stream to its end issues the credit.
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();
	assert_matches!(poll_read(&mut pair, server_ch, s, &waiter), Poll::Ready(Ok(Some(_))));
	assert_matches!(poll_read(&mut pair, server_ch, s, &waiter), Poll::Ready(Ok(None)));
	pair.drive();
	assert_eq!(waiter.wakes(), 1);
	let poll = pair
		.client_streams(client_ch)
		.poll_open(&waiter, Dir::Uni, Default::default());
	assert_matches!(poll, Poll::Ready(Ok(_)));
}

#[test]
fn write_waits_for_room() {
	let _guard = subscribe();
	let mut pair = pair_with(Limits {
		stream_receive_window: NonZeroU64::new(10),
		..Limits::default()
	});
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, &[0; 10]);

	let waiter = Counter::new();
	assert!(
		pair.client_send(client_ch, s)
			.poll_write(&waiter, &mut &[1; 10][..])
			.is_pending()
	);

	// Reading frees the window, which the peer hears of in MAX_STREAM_DATA.
	assert_matches!(poll_read(&mut pair, server_ch, s, &waiter), Poll::Ready(Ok(Some(_))));
	pair.drive();
	assert_eq!(waiter.wakes(), 1);
	assert_matches!(
		pair.client_send(client_ch, s).poll_write(&waiter, &mut &[1; 10][..]),
		Poll::Ready(Ok(n)) if n > 0
	);
}

#[test]
fn write_waits_for_the_connection_window() {
	let _guard = subscribe();
	let mut pair = pair_with(Limits {
		receive_window: NonZeroU64::new(10),
		..Limits::default()
	});
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, &[0; 10]);

	let waiter = Counter::new();
	assert!(
		pair.client_send(client_ch, s)
			.poll_write(&waiter, &mut &[1; 10][..])
			.is_pending()
	);

	assert_matches!(poll_read(&mut pair, server_ch, s, &waiter), Poll::Ready(Ok(Some(_))));
	pair.drive();
	assert_eq!(waiter.wakes(), 1);
	assert_matches!(
		pair.client_send(client_ch, s).poll_write(&waiter, &mut &[1; 10][..]),
		Poll::Ready(Ok(n)) if n > 0
	);
}

#[test]
fn end_waits_for_the_finish_to_be_acked() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let (end, acked) = (Counter::new(), Counter::new());
	let mut send = pair.client_send(client_ch, s);
	let offset = send.offset();
	send.write(&mut &b"b"[..]).unwrap();
	let tail = send.offset();
	send.finish().unwrap();
	assert!(send.poll_end(&end).is_pending());
	// Already acknowledged, so ready at once.
	assert_eq!(send.poll_acked(&acked, offset), Poll::Ready(Ok(())));
	assert!(send.poll_acked(&acked, tail).is_pending());

	pair.drive();
	assert_eq!((end.wakes(), acked.wakes()), (1, 1));
	let mut send = pair.client_send(client_ch, s);
	assert_eq!(send.poll_end(&end), Poll::Ready(Ok(send::End::Finished)));
	assert_eq!(send.poll_acked(&acked, tail), Poll::Ready(Ok(())));
}

#[test]
fn end_hears_a_stop() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let (end, write) = (Counter::new(), Counter::new());
	assert!(pair.client_send(client_ch, s).poll_end(&end).is_pending());
	pair.server_recv(server_ch, s).stop(VarInt(7)).unwrap();
	pair.drive();
	assert_eq!(end.wakes(), 1);
	assert_eq!(
		pair.client_send(client_ch, s).poll_end(&end),
		Poll::Ready(Ok(send::End::Stopped(VarInt(7))))
	);
	assert_eq!(
		pair.client_send(client_ch, s).poll_write(&write, &mut &b"b"[..]),
		Poll::Ready(Err(send::Error::Stopped(VarInt(7))))
	);
}

#[test]
fn end_hears_a_local_reset() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let end = Counter::new();
	assert!(pair.client_send(client_ch, s).poll_end(&end).is_pending());
	pair.client_send(client_ch, s).reset(VarInt(3)).unwrap();
	assert_eq!(end.wakes(), 1);
	assert_eq!(
		pair.client_send(client_ch, s).poll_end(&end),
		Poll::Ready(Ok(send::End::Reset(VarInt(3))))
	);
}

/// Releasing the ID wakes every send wait on it, which then finds it closed.
#[test]
fn release_wakes_send_waiters() {
	let _guard = subscribe();
	let mut pair = pair_with(Limits {
		stream_receive_window: NonZeroU64::new(10),
		..Limits::default()
	});
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, &[0; 5]);

	let (write, end, acked) = (Counter::new(), Counter::new(), Counter::new());
	let mut send = pair.client_send(client_ch, s);
	// Fill the window with bytes not yet sent, let alone acknowledged.
	send.write(&mut &[0; 5][..]).unwrap();
	let tail = send.offset();
	assert!(send.poll_write(&write, &mut &[1; 10][..]).is_pending());
	assert!(send.poll_end(&end).is_pending());
	assert!(send.poll_acked(&acked, tail).is_pending());
	send.release();
	assert_eq!((write.wakes(), end.wakes(), acked.wakes()), (1, 1, 1));

	let mut send = pair.client_send(client_ch, s);
	assert_eq!(
		send.poll_write(&write, &mut &[1; 10][..]),
		Poll::Ready(Err(send::Error::Closed))
	);
	assert_eq!(send.poll_end(&end), Poll::Ready(Err(send::Error::Closed)));
	assert_eq!(send.poll_acked(&acked, tail), Poll::Ready(Err(send::Error::Closed)));
	assert_eq!(pair.client_conn_mut(client_ch).waiting_streams(), 0);
}

/// A local finish wakes a writer parked on a full window, which then finds the stream closed.
#[test]
fn finish_wakes_a_blocked_writer() {
	let _guard = subscribe();
	let mut pair = pair_with(Limits {
		stream_receive_window: NonZeroU64::new(10),
		..Limits::default()
	});
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, &[0; 10]);

	let waiter = Counter::new();
	assert!(
		pair.client_send(client_ch, s)
			.poll_write(&waiter, &mut &[1; 10][..])
			.is_pending()
	);
	pair.client_send(client_ch, s).finish().unwrap();
	assert_eq!(waiter.wakes(), 1);
	assert_eq!(
		pair.client_send(client_ch, s).poll_write(&waiter, &mut &[1; 10][..]),
		Poll::Ready(Err(send::Error::Closed))
	);
}

#[test]
fn datagram_waits_for_arrival() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let waiter = Counter::new();
	assert!(pair.server_datagrams(server_ch).poll_recv(&waiter).is_pending());
	pair.client_datagrams(client_ch)
		.send(&b"hi"[..], Default::default())
		.unwrap();
	pair.drive();
	assert_eq!(waiter.wakes(), 1);
	assert_matches!(pair.server_datagrams(server_ch).poll_recv(&waiter), Poll::Ready(Ok(data)) if data == "hi");
}

#[test]
fn handshake_waits() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let client_ch = pair.begin_connect(client_config());

	let (connected, confirmed) = (Counter::new(), Counter::new());
	let conn = pair.client_conn_mut(client_ch);
	assert!(conn.poll_connected(&connected).is_pending());
	assert!(conn.poll_handshake_confirmed(&confirmed).is_pending());

	pair.drive();
	assert_eq!((connected.wakes(), confirmed.wakes()), (1, 1));
	let now = pair.time;
	let conn = pair.client_conn_mut(client_ch);
	assert_eq!(conn.poll_connected(&connected), Poll::Ready(Ok(())));
	assert_eq!(conn.poll_handshake_confirmed(&confirmed), Poll::Ready(Ok(())));

	// Still connected once lost: the handshake did complete.
	conn.close(now, VarInt(0), String::new());
	assert_eq!(conn.poll_connected(&connected), Poll::Ready(Ok(())));
}

/// What arrived before the loss stays available, then every wait fails with the reason.
#[test]
fn lost_drains_then_fails() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();

	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &b"a"[..]).unwrap();
	pair.client_datagrams(client_ch)
		.send(&b"hi"[..], Default::default())
		.unwrap();
	pair.drive();

	let (closed, bi, write) = (Counter::new(), Counter::new(), Counter::new());
	let server = pair.server_conn_mut(server_ch);
	assert!(server.poll_closed(&closed).is_pending());
	assert!(server.streams().poll_accept(&bi, Dir::Bi).is_pending());
	let ours = server.streams().open(Dir::Uni, Default::default()).unwrap();
	assert!(server.send_stream(ours).poll_end(&write).is_pending());

	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(42), "bye".into());
	pair.drive();
	assert_eq!((closed.wakes(), bi.wakes(), write.wakes()), (1, 1, 1));

	let server = pair.server_conn_mut(server_ch);
	let reason = assert_matches!(server.poll_closed(&closed), Poll::Ready(reason) => reason);
	assert_matches!(&reason, ConnectionError::ApplicationClosed(close) if close.error_code == VarInt(42));
	assert_eq!(server.streams().poll_accept(&bi, Dir::Uni), Poll::Ready(Ok(s)));
	assert_eq!(
		server.streams().poll_accept(&bi, Dir::Uni),
		Poll::Ready(Err(reason.clone()))
	);
	assert_eq!(
		server.streams().poll_accept(&bi, Dir::Bi),
		Poll::Ready(Err(reason.clone()))
	);
	let poll = server.streams().poll_open(&bi, Dir::Uni, Default::default());
	assert_eq!(poll, Poll::Ready(Err(reason.clone())));
	assert_eq!(
		server.send_stream(ours).poll_end(&write),
		Poll::Ready(Err(send::Error::ConnectionLost))
	);
	assert_matches!(server.datagrams().poll_recv(&bi), Poll::Ready(Ok(data)) if data == "hi");
	assert_eq!(server.datagrams().poll_recv(&bi), Poll::Ready(Err(reason)));

	assert_matches!(poll_read(&mut pair, server_ch, s, &bi), Poll::Ready(Ok(Some(data))) if data == "a");
	assert_eq!(
		poll_read(&mut pair, server_ch, s, &bi),
		Poll::Ready(Err(recv::Error::ConnectionLost))
	);
}

/// A local close wakes a writer parked on a full window, which then learns the connection is gone.
#[test]
fn local_close_wakes_a_blocked_writer() {
	let _guard = subscribe();
	let mut pair = pair_with(Limits {
		stream_receive_window: NonZeroU64::new(10),
		..Limits::default()
	});
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, &[0; 10]);

	let waiter = Counter::new();
	assert!(
		pair.client_send(client_ch, s)
			.poll_write(&waiter, &mut &[1; 10][..])
			.is_pending()
	);
	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(0), String::new());
	assert_eq!(waiter.wakes(), 1);
	assert_eq!(
		pair.client_send(client_ch, s).poll_write(&waiter, &mut &[1; 10][..]),
		Poll::Ready(Err(send::Error::ConnectionLost))
	);
}

/// A local stop wakes a reader parked on the stream, which then finds it closed.
#[test]
fn stop_wakes_a_parked_reader() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = open_uni(&mut pair, client_ch, server_ch, b"a");

	let waiter = Counter::new();
	assert!(poll_read(&mut pair, server_ch, s, &waiter).is_ready());
	assert!(poll_read(&mut pair, server_ch, s, &waiter).is_pending());
	pair.server_recv(server_ch, s).stop(VarInt(1)).unwrap();
	assert_eq!(waiter.wakes(), 1);
	assert!(matches!(
		pair.server_recv(server_ch, s).read(),
		Err(recv::Error::Closed)
	));
}
