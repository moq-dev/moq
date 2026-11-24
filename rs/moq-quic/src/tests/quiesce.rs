//! Tests for `poll_quiesce`, which asks the peer to end every stream it sends on and waits for it

use super::*;

const CODE: VarInt = VarInt(9);

/// Open a client stream in `dir` carrying `data` and accept it on the server
fn open(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle, dir: Dir) -> StreamId {
	let s = pair.client_streams(client_ch).open(dir, Default::default()).unwrap();
	pair.client_send(client_ch, s).write(&mut &b"in flight"[..]).unwrap();
	pair.drive();
	assert_eq!(pair.server_streams(server_ch).accept(dir), Some(s));
	s
}

fn quiesce(
	pair: &mut Pair,
	ch: ConnectionHandle,
	waiter: &Counter,
	keep: &[StreamId],
) -> Poll<Result<(), ConnectionError>> {
	pair.server_conn_mut(ch).poll_quiesce(waiter, CODE, keep)
}

/// Reset each client stream the server stopped, as a peer answers STOP_SENDING
fn answer(pair: &mut Pair, ch: ConnectionHandle, streams: &[StreamId]) {
	for &s in streams {
		assert_eq!(pair.client_send(ch, s).end(), Ok(Some(send::End::Stopped(CODE))));
		pair.client_send(ch, s).reset(CODE).unwrap();
	}
	pair.drive();
}

fn stops_sent(pair: &mut Pair, ch: ConnectionHandle) -> u64 {
	pair.server_conn_mut(ch).stats().frame_tx.stop_sending
}

/// Every stream the peer writes is stopped once, those it opens during the wait included once
/// accepted, and the wait ends with their resets while the kept stream and the readers stay
/// untouched.
#[test]
fn waits_for_every_reset() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let uni = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let bi = open(&mut pair, client_ch, server_ch, Dir::Bi);
	let kept = open(&mut pair, client_ch, server_ch, Dir::Uni);

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[kept]).is_pending());
	// A stream opened before the stops arrive waits for the application to accept it, and is
	// asked once it has.
	let late = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, late).write(&mut &b"late"[..]).unwrap();
	pair.drive();
	assert!(waiter.wakes() > 0);
	assert!(quiesce(&mut pair, server_ch, &waiter, &[kept]).is_pending());
	pair.drive();
	assert_eq!(
		stops_sent(&mut pair, server_ch),
		2,
		"an unaccepted stream is left to the application"
	);
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(late));
	assert!(quiesce(&mut pair, server_ch, &waiter, &[kept]).is_pending());
	pair.drive();

	let wakes = waiter.wakes();
	answer(&mut pair, client_ch, &[uni, bi]);
	assert!(waiter.wakes() > wakes);
	assert!(
		quiesce(&mut pair, server_ch, &waiter, &[kept]).is_pending(),
		"the late stream is still open"
	);
	answer(&mut pair, client_ch, &[late]);
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[kept]), Poll::Ready(Ok(())));
	assert_eq!(stops_sent(&mut pair, server_ch), 3, "each stream is asked once");
	assert_eq!(pair.client_send(client_ch, kept).end(), Ok(None));
	// The reader still holds its stream, and learns of the reset by reading.
	let mut recv = pair.server_recv(server_ch, uni);
	let mut chunks = recv.read().unwrap();
	while let Poll::Ready(Ok(Some(_))) = chunks.try_next(usize::MAX) {}
	chunks.finalize();
	assert_eq!(recv.received_reset(), Ok(Some(CODE)));
}

/// A stream the peer already finished, or one the reader stopped and the peer ended, holds
/// nothing up and is asked nothing more.
#[test]
fn ended_streams_are_done() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let finished = open(&mut pair, client_ch, server_ch, Dir::Uni);
	pair.client_send(client_ch, finished).finish().unwrap();
	let stopped = open(&mut pair, client_ch, server_ch, Dir::Uni);
	pair.server_recv(server_ch, stopped).stop(VarInt(1)).unwrap();
	pair.drive();
	pair.client_send(client_ch, stopped).reset(VarInt(1)).unwrap();
	pair.drive();
	let before = stops_sent(&mut pair, server_ch);

	let waiter = Counter::new();
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Ok(())));
	pair.drive();
	assert_eq!(stops_sent(&mut pair, server_ch), before);
}

/// The wait fails once the connection is lost, waking the waiter.
#[test]
fn fails_on_loss() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	open(&mut pair, client_ch, server_ch, Dir::Uni);

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	let now = pair.time;
	pair.client_conn_mut(client_ch).close(now, VarInt(3), String::new());
	pair.drive();
	assert!(waiter.wakes() > 0);
	assert_matches!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Err(_)));
}
