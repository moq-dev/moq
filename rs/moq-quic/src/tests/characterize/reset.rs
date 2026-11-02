//! How a reset or stopped stream ends for the reader and what credit it gives back

use super::*;

const CODE: VarInt = VarInt(7);

/// Every chunk a server stream yields, and how it ended
fn read_all(pair: &mut Pair, ch: ConnectionHandle, s: StreamId) -> (Vec<u8>, recv::Error) {
	let mut recv = pair.server_recv(ch, s);
	let mut chunks = recv.read().unwrap();
	let mut data = Vec::new();
	let end = loop {
		match chunks.next(usize::MAX) {
			Ok(Some(chunk)) => data.extend_from_slice(&chunk.bytes),
			Ok(None) => panic!("stream finished"),
			Err(e) => break e,
		}
	};
	let _ = chunks.finalize();
	(data, end)
}

/// Opens a client uni stream carrying `data`, delivered and accepted by the server
fn open(pair: &mut Pair, client_ch: ConnectionHandle, server_ch: ConnectionHandle, data: &[u8]) -> StreamId {
	let s = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	pair.client_send(client_ch, s).write(&mut &data[..]).unwrap();
	pair.drive();
	assert_eq!(pair.server_streams(server_ch).accept(Dir::Uni), Some(s));
	s
}

#[test]
fn received_reset_reports_the_code_once() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();
	let s = open(&mut pair, client_ch, server_ch, b"hello");
	assert_eq!(pair.server_recv(server_ch, s).received_reset(), Ok(None));

	pair.client_send(client_ch, s).reset(CODE).unwrap();
	pair.drive();

	// Observing the reset frees the stream, so every later access finds it closed
	assert_eq!(pair.server_recv(server_ch, s).received_reset(), Ok(Some(CODE)));
	assert!(pair.server_recv(server_ch, s).received_reset().is_err());
	assert_eq!(pair.server_recv(server_ch, s).read().err(), Some(recv::Error::Closed));
	assert!(pair.server_recv(server_ch, s).stop(CODE).is_err());
}

#[test]
fn received_reset_on_closed_streams() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();

	// Never opened
	let unopened = StreamId::new(Side::Client, Dir::Uni, 5);
	assert!(pair.server_recv(server_ch, unopened).received_reset().is_err());

	// Stopped by the reader, even after the peer resets in response
	let s = open(&mut pair, client_ch, server_ch, b"hello");
	pair.server_recv(server_ch, s).stop(CODE).unwrap();
	assert!(pair.server_recv(server_ch, s).received_reset().is_err());
	pair.drive();
	assert!(pair.server_recv(server_ch, s).received_reset().is_err());

	// Finished and fully read
	let s = open(&mut pair, client_ch, server_ch, b"hello");
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();
	assert_eq!(pair.server_recv(server_ch, s).received_reset(), Ok(None));
	{
		let mut recv = pair.server_recv(server_ch, s);
		let mut chunks = recv.read().unwrap();
		assert!(chunks.next(usize::MAX).unwrap().is_some());
		assert_eq!(chunks.next(usize::MAX), Ok(None));
		let _ = chunks.finalize();
	}
	assert!(pair.server_recv(server_ch, s).received_reset().is_err());
}

/// A plain RESET_STREAM discards whatever the reader has not read yet, delivered or not
#[test]
fn chunks_end_in_reset() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();
	let s = open(&mut pair, client_ch, server_ch, b"hello");

	// Part of the stream is read before the reset arrives
	{
		let mut recv = pair.server_recv(server_ch, s);
		let mut chunks = recv.read().unwrap();
		assert_eq!(chunks.next(2).unwrap().unwrap().bytes, &b"he"[..]);
		let _ = chunks.finalize();
	}
	pair.client_send(client_ch, s).reset(CODE).unwrap();
	pair.drive();

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_eq!(chunks.next(usize::MAX), Err(recv::Error::Reset(CODE)));
	// The end repeats until the chunks are finalized
	assert_eq!(chunks.next(usize::MAX), Err(recv::Error::Reset(CODE)));
	let _ = chunks.finalize();
	assert_eq!(pair.server_recv(server_ch, s).read().err(), Some(recv::Error::Closed));
}

#[test]
fn chunks_end_in_finished() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();
	let s = open(&mut pair, client_ch, server_ch, b"hello");
	pair.client_send(client_ch, s).finish().unwrap();
	pair.drive();

	let mut recv = pair.server_recv(server_ch, s);
	let mut chunks = recv.read().unwrap();
	assert_eq!(chunks.next(usize::MAX).unwrap().unwrap().bytes, &b"hello"[..]);
	assert_eq!(chunks.next(usize::MAX), Ok(None));
	assert_eq!(chunks.next(usize::MAX), Ok(None));
	let _ = chunks.finalize();
	assert_eq!(pair.server_recv(server_ch, s).read().err(), Some(recv::Error::Closed));
}

/// The reader sees exactly the code the writer reset with, across the varint range
#[test]
fn reset_codes() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected();
	for code in [VarInt(0), VarInt(63), VarInt(64), VarInt::MAX] {
		let s = open(&mut pair, client_ch, server_ch, b"x");
		pair.client_send(client_ch, s).reset(code).unwrap();
		pair.drive();
		assert_eq!(
			read_all(&mut pair, server_ch, s),
			(Vec::new(), recv::Error::Reset(code))
		);

		let s = open(&mut pair, client_ch, server_ch, b"x");
		pair.server_recv(server_ch, s).stop(code).unwrap();
		pair.drive();
		assert_eq!(
			pair.client_send(client_ch, s).write(&mut &b"y"[..]),
			Err(send::Error::Stopped(code))
		);
	}
}

/// Stream ID credit for a reset stream returns only once the reader observes the reset
#[test]
fn reset_returns_stream_credit_once_observed() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch) = connected_with(|t| t.limits.max_streams = NonZeroU64::new(1), |_| {});
	let s = open(&mut pair, client_ch, server_ch, b"hello");
	pair.client_send(client_ch, s).reset(CODE).unwrap();
	pair.drive();
	assert_eq!(pair.client_streams(client_ch).open(Dir::Uni, Default::default()), None);

	assert_eq!(read_all(&mut pair, server_ch, s).1, recv::Error::Reset(CODE));
	pair.drive();
	assert!(
		pair.client_streams(client_ch)
			.open(Dir::Uni, Default::default())
			.is_some()
	);
}

/// The connection window used by `s`'s unread bytes, which fill it
const WINDOW: u32 = 4000;

/// A connected pair whose server's connection window is filled by an unread client stream
fn blocked() -> (Pair, ConnectionHandle, ConnectionHandle, StreamId) {
	let (mut pair, client_ch, server_ch) =
		connected_with(|t| t.limits.receive_window = NonZeroU64::new(WINDOW.into()), |_| {});
	let s = open(&mut pair, client_ch, server_ch, &[0; WINDOW as usize]);
	// The window is spent, so nothing more can be written
	let other = pair
		.client_streams(client_ch)
		.open(Dir::Uni, Default::default())
		.unwrap();
	assert_eq!(
		pair.client_send(client_ch, other).write(&mut &b"x"[..]),
		Err(send::Error::Blocked)
	);
	(pair, client_ch, server_ch, s)
}

/// How many bytes a fresh client stream can write
fn writable(pair: &mut Pair, ch: ConnectionHandle) -> usize {
	let s = pair.client_streams(ch).open(Dir::Uni, Default::default()).unwrap();
	pair.client_send(ch, s)
		.write(&mut &[0; 2 * WINDOW as usize][..])
		.unwrap_or(0)
}

#[test]
fn reset_returns_connection_credit() {
	let _guard = subscribe();
	let (mut pair, client_ch, _, s) = blocked();
	pair.client_send(client_ch, s).reset(CODE).unwrap();
	pair.drive();
	assert_eq!(writable(&mut pair, client_ch), WINDOW as usize);
}

#[test]
fn stop_returns_connection_credit() {
	let _guard = subscribe();
	let (mut pair, client_ch, server_ch, s) = blocked();
	pair.server_recv(server_ch, s).stop(CODE).unwrap();
	pair.drive();
	assert_eq!(writable(&mut pair, client_ch), WINDOW as usize);
}
