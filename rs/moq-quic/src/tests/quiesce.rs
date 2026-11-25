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

/// Writers that reset and release their streams once the quiesce stopped them, as dropped
/// handles echo a stop, still deliver what they wrote, and no stream ends, theirs included, until
/// the peer acknowledged everything written.
#[test]
fn a_writers_reset_keeps_what_it_wrote() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let theirs = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let delivered = open_ours(&mut pair, server_ch);
	let refusal = pair
		.server_streams(server_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.server_send(server_ch, refusal)
		.write(&mut &b"refused"[..])
		.unwrap();

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	for s in [delivered, refusal] {
		pair.server_send(server_ch, s).reset(VarInt(5)).unwrap();
		pair.server_send(server_ch, s).release();
	}
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive_server();
	assert_eq!(stops_sent(&mut pair, server_ch), 0);
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_tx.reset_stream, 0);

	pair.drive();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive();
	assert_eq!(stops_sent(&mut pair, server_ch), 1);
	assert_eq!(
		reset_seen(&mut pair, client_ch, refusal),
		(b"refused".to_vec(), Some(VarInt(5)))
	);
	assert_eq!(
		reset_seen(&mut pair, client_ch, delivered),
		(b"unread".to_vec(), Some(VarInt(5)))
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

/// Write to each of `streams` what the client loses, so it stays unacknowledged
fn lose_writes(pair: &mut Pair, server_ch: ConnectionHandle, streams: &[StreamId]) {
	for &s in streams {
		pair.server_send(server_ch, s).write(&mut &b"lost"[..]).unwrap();
	}
	pair.drive_server();
	pair.client.inbound.clear();
}

/// Deliver the client's STOP_SENDING for each of `streams` to the server, and nothing else the
/// server sends
fn stop_from_client(pair: &mut Pair, client_ch: ConnectionHandle, streams: &[StreamId]) {
	for &s in streams {
		pair.client_recv(client_ch, s).stop(VarInt(1)).unwrap();
	}
	pair.drive_client();
	pair.drive_server();
	pair.client.inbound.clear();
}

/// A reset that drops the last bytes the peer has yet to acknowledge, as one does once the peer
/// stopped the stream, wakes the wait instead of leaving it to the grace: a writer's reset after
/// a STOP_SENDING that came before the quiesce, and a peer's STOP_SENDING after the writer's reset.
#[test]
fn a_reset_dropping_unacked_data_wakes_the_wait() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let stopped_first = open_ours(&mut pair, server_ch);
	let reset_first = open_ours(&mut pair, server_ch);
	lose_writes(&mut pair, server_ch, &[stopped_first, reset_first]);
	stop_from_client(&mut pair, client_ch, &[stopped_first]);

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	let wakes = waiter.wakes();
	pair.server_send(server_ch, stopped_first).reset(VarInt(5)).unwrap();
	assert!(waiter.wakes() > wakes, "the writer's reset left the wait asleep");
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());

	pair.server_send(server_ch, reset_first).reset(VarInt(5)).unwrap();
	let wakes = waiter.wakes();
	stop_from_client(&mut pair, client_ch, &[reset_first]);
	assert!(waiter.wakes() > wakes, "the peer's stop left the wait asleep");
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	pair.drive();
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Ok(())));
}

/// Abandoning a quiesce that is not ready resets the streams it stopped, sends the resets their
/// writers asked for, and stops the peer's streams, all of which it held for the peer to
/// acknowledge what was written.
#[test]
fn abandoning_sends_the_held_resets_and_stops() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let theirs = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let kept = open(&mut pair, client_ch, server_ch, Dir::Uni);
	let reset = open_ours(&mut pair, server_ch);
	let open = open_ours(&mut pair, server_ch);
	lose_writes(&mut pair, server_ch, &[reset]);

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[kept]).is_pending());
	pair.server_send(server_ch, reset).reset(VarInt(5)).unwrap();
	pair.drive_server();
	pair.client.inbound.clear();
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_tx.reset_stream, 0);
	assert_eq!(stops_sent(&mut pair, server_ch), 0);

	pair.server_conn_mut(server_ch).abandon_quiesce(CODE, &[kept]);
	pair.drive();
	assert_eq!(reset_seen(&mut pair, client_ch, reset).1, Some(VarInt(5)));
	assert_eq!(reset_seen(&mut pair, client_ch, open), (b"unread".to_vec(), Some(CODE)));
	assert_eq!(
		pair.client_send(client_ch, theirs).end(),
		Ok(Some(send::End::Stopped(CODE)))
	);
	assert_eq!(pair.client_send(client_ch, kept).end(), Ok(None));
	// Nothing is held any longer: the stream the quiesce stopped is already reset.
	assert_eq!(
		pair.server_send(server_ch, open).reset(VarInt(5)),
		Err(send::Error::Closed)
	);
}

/// A stream the quiesce stopped and nobody reset is reset at once, keeping nothing, when the peer
/// stops it too: the peer reads nothing more, and nothing is left for an abandon to end.
#[test]
fn a_peers_stop_resets_a_quiesced_stream() {
	let _guard = subscribe();
	let mut pair = Pair::default();
	let (client_ch, server_ch) = pair.connect();
	let ours = open_ours(&mut pair, server_ch);
	lose_writes(&mut pair, server_ch, &[ours]);

	let waiter = Counter::new();
	assert!(quiesce(&mut pair, server_ch, &waiter, &[]).is_pending());
	let wakes = waiter.wakes();
	stop_from_client(&mut pair, client_ch, &[ours]);
	assert!(waiter.wakes() > wakes, "the reset left the wait asleep");
	assert_eq!(pair.server_conn_mut(server_ch).stats().frame_tx.reset_stream, 1);
	// Its writer sees the peer's stop, and has nothing left to reset.
	assert_eq!(pair.server_send(server_ch, ours).stopped(), Ok(Some(VarInt(1))));
	assert_eq!(
		pair.server_send(server_ch, ours).reset(VarInt(5)),
		Err(send::Error::Closed)
	);
	pair.drive();
	assert_eq!(quiesce(&mut pair, server_ch, &waiter, &[]), Poll::Ready(Ok(())));
	assert!(
		pair.client_conn_mut(client_ch).stats().frame_rx.reset_stream > 0,
		"the reset never arrived"
	);
}
