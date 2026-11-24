//! Tests for `poll_quiesce`, which ends every stream either side sends on and waits for the peer

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

/// Open a server stream carrying data the client has yet to read
fn open_ours(pair: &mut Pair, server_ch: ConnectionHandle) -> StreamId {
	let s = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.server_send(server_ch, s).write(&mut &b"unread"[..]).unwrap();
	pair.drive();
	s
}

/// What the client's reader of `s` reads, and the code it learns the stream was reset with, if
/// it was
fn reset_seen(pair: &mut Pair, client_ch: ConnectionHandle, s: StreamId) -> (Vec<u8>, Option<VarInt>) {
	let mut recv = pair.client_recv(client_ch, s);
	let mut chunks = recv.read().unwrap();
	let mut data = Vec::new();
	while let Poll::Ready(Ok(Some(chunk))) = chunks.try_next(usize::MAX) {
		data.extend_from_slice(&chunk.bytes);
	}
	chunks.finalize();
	(data, recv.received_reset().unwrap())
}

/// Every stream the server writes is reset, and every stream the peer writes is stopped once,
/// those it opens during the wait included once accepted, and the wait ends with their resets
/// while the kept streams and the readers stay untouched.
#[test]
fn waits_for_every_reset() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let uni = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let bi = open(&mut pair, client_ch, server_ch, Dir::Bi);
	let kept = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let ours = open_ours(&mut pair, server_ch);
	let ours_kept = open_ours(&mut pair, server_ch);
	let keep = [kept, ours_kept];

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &keep).is_pending());
	pair.drive();
	// The streams the server sends on are reset at once, the bidirectional one's half included,
	// still delivering what was written.
	assert_eq!(reset_seen(&mut pair, client_ch, ours), (b"unread".to_vec(), Some(CODE)));
	assert_eq!(reset_seen(&mut pair, client_ch, bi), (Vec::new(), Some(CODE)));
	assert_eq!(reset_seen(&mut pair, client_ch, ours_kept), (b"unread".to_vec(), None));
	// Their writers see them stopped, as if by the peer.
	assert_eq!(pair.server_send(server_ch, ours).stopped(), Ok(Some(CODE)));
	assert_eq!(pair.server_send(server_ch, ours_kept).stopped(), Ok(None));
	// A stream opened before the stops arrive waits for the application to accept it, and is
	// asked once it has.
	let late = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, late).write(&mut &b"late"[..]).unwrap();
	pair.drive();
	assert!(waiter.wakes() > 0);
	assert!(quiesce(&mut pair, server_ch, &waiter, &keep).is_pending());
	pair.drive();
	assert_eq!(
		stops_sent(&mut pair, server_ch),
		2,
		"an unaccepted stream is left to the application"
	);
	// Accepting it wakes the wait, whose next poll stops it.
	let wakes = waiter.wakes();
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(late));
	assert!(waiter.wakes() > wakes);
	assert!(quiesce(&mut pair, server_ch, &waiter, &keep).is_pending());
	pair.drive();

	let wakes = waiter.wakes();
	answer(&mut pair, client_ch, &[uni, bi]);
	assert!(waiter.wakes() > wakes);
	assert!(
		quiesce(&mut pair, server_ch, &waiter, &keep).is_pending(),
		"the late stream is still open"
	);
	answer(&mut pair, client_ch, &[late]);
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &keep), Poll::Ready(Ok(())));
	assert_eq!(stops_sent(&mut pair, server_ch), 3, "each stream is asked once");
	assert_eq!(pair.client_send(client_ch, kept).end(), Ok(None));
	// The reader still holds its stream, and learns of the reset by reading.
	let mut recv = pair.server_recv(server_ch, uni);
	let mut chunks = recv.read().unwrap();
	while let Poll::Ready(Ok(Some(_))) = chunks.try_next(usize::MAX) {}
	chunks.finalize();
	assert_eq!(recv.received_reset(), Ok(Some(CODE)));
}

/// What the server wrote reaches the peer before any stream ends, whatever its packets do: the
/// writers stop at once, and the resets and stops wait until the peer acknowledged the data.
#[test]
fn writes_arrive_before_anything_ends() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let theirs = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let ours = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.server_send(server_ch, ours).write(&mut &b"refused"[..]).unwrap();

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	assert_eq!(pair.server_send(server_ch, ours).stopped(), Ok(Some(CODE)));
	// The data is sent but not yet acknowledged: nothing ends.
	pair.drive_server();
	assert_eq!(stops_sent(&mut pair, server_ch), 0);
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_tx.reset_stream, 0);

	pair.drive();
	assert!(waiter.wakes() > 0);
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive();
	assert_eq!(stops_sent(&mut pair, server_ch), 1);
	assert_eq!(
		reset_seen(&mut pair, client_ch, ours),
		(b"refused".to_vec(), Some(CODE))
	);
	answer(&mut pair, client_ch, &[theirs]);
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Ok(())));
}

/// A writer that resets its stream once the quiesce stopped it, as a dropped handle echoes a
/// stop, still delivers what it wrote, and nothing else ends until the peer acknowledged it.
#[test]
fn a_writers_reset_keeps_what_it_wrote() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let theirs = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let ours = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.server_send(server_ch, ours).write(&mut &b"refused"[..]).unwrap();

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.server_send(server_ch, ours).reset(VarInt(5)).unwrap();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive_server();
	assert_eq!(stops_sent(&mut pair, server_ch), 0);

	pair.drive();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive();
	assert_eq!(stops_sent(&mut pair, server_ch), 1);
	assert_eq!(
		reset_seen(&mut pair, client_ch, ours),
		(b"refused".to_vec(), Some(VarInt(5)))
	);
	answer(&mut pair, client_ch, &[theirs]);
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Ok(())));
}

/// A writer parked on its stream wakes when the quiesce stops it, before the data it wrote lets
/// the stream be reset.
#[test]
fn parked_writers_wake() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (_, server_ch) = pair.connect();
	let ours = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.server_send(server_ch, ours).write(&mut &b"unacked"[..]).unwrap();
	let writer = Counter::new();
	assert!(pair.server_send(server_ch, ours).poll_stopped(&writer).is_pending());

	assert!(quiesce(&mut pair, server_ch, &Counter::new(), &[]).is_pending());
	assert!(writer.wakes() > 0);
	assert_eq!(
		pair.server_send(server_ch, ours).poll_stopped(&writer),
		Poll::Ready(Ok(Some(CODE)))
	);
}

/// With nothing left for the peer to end, the wait lasts until it acknowledged the resets.
#[test]
fn waits_for_its_resets_to_be_acknowledged() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let ours = open_ours(&mut pair, server_ch);

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive();
	assert!(waiter.wakes() > 0);
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Ok(())));
	assert_eq!(reset_seen(&mut pair, client_ch, ours), (b"unread".to_vec(), Some(CODE)));
}

/// A stream its application finished and released is left to finish, and the wait lasts until
/// the peer acknowledged it.
#[test]
fn released_streams_finish() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let s = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.server_send(server_ch, s).write(&mut &b"done"[..]).unwrap();
	pair.server_send(server_ch, s).finish().unwrap();
	pair.server_send(server_ch, s).release();

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive();
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Ok(())));
	let mut recv = pair.client_recv(client_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_matches!(chunks.next(usize::MAX), Ok(Some(chunk)) if chunk.bytes == b"done"[..]);
	assert_matches!(chunks.next(usize::MAX), Ok(None));
	chunks.finalize();
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
