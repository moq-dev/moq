use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::*;
use crate::Version;

async fn recv(stream: &mut Stream<DuplexStream>) -> Result<Bytes, Error> {
	kio::wait(|waiter| stream.poll_recv(waiter)).await
}

/// Poll `recv` once, for a record that must not be complete yet.
fn try_recv(stream: &mut Stream<DuplexStream>) -> Poll<Result<Bytes, Error>> {
	stream.poll_recv(&kio::Waiter::noop())
}

#[test]
fn wrapping_a_stream_does_not_start_io() {
	let (io, _peer) = tokio::io::duplex(64);
	let _transport = Stream::new(io);
}

#[tokio::test(start_paused = true)]
async fn records_carry_their_size_field() {
	let (io, mut peer) = tokio::io::duplex(1024);
	let mut stream = bound(io, Version::QMux02, 16 * 1024);
	stream.send(Bytes::from_static(b"hello")).unwrap();
	stream.send(Bytes::from(vec![7; 100])).unwrap();
	kio::wait(|waiter| stream.poll_flush(waiter)).await.unwrap();

	let mut wire = vec![0; 1 + 5 + 2 + 100];
	peer.read_exact(&mut wire).await.unwrap();
	assert_eq!(&wire[..6], b"\x05hello");
	assert_eq!(&wire[6..8], &[0x40, 100]);
	assert!(wire[8..].iter().all(|&b| b == 7));
}

/// A record dripping in one byte at a time stays pending until its last byte, then arrives
/// whole.
#[tokio::test]
async fn a_record_split_across_reads_arrives_whole() {
	for (version, wire, payload) in [
		// 1-byte size (0x08) followed by 8 bytes of payload.
		(Version::QMux01, &b"\x08abcdefgh"[..], &b"abcdefgh"[..]),
		// STREAM with LEN (0x0a), id 4, length 5, "hello": draft-00 hands over the frame.
		(Version::QMux00, &b"\x0a\x04\x05hello"[..], &b"\x0a\x04\x05hello"[..]),
	] {
		let (io, mut peer) = tokio::io::duplex(64 * 1024);
		let mut stream = bound(io, version, 16 * 1024);
		for byte in &wire[..wire.len() - 1] {
			peer.write_all(&[*byte]).await.unwrap();
			tokio::task::yield_now().await;
			assert!(try_recv(&mut stream).is_pending(), "{version:?}: a partial record arrived");
		}
		peer.write_all(&wire[wire.len() - 1..]).await.unwrap();
		assert_eq!(recv(&mut stream).await.unwrap(), payload, "{version:?}");
	}
}

/// Records that share one read come out in order, as slices of that read.
#[tokio::test]
async fn records_in_one_read_arrive_in_order() {
	let (io, mut peer) = tokio::io::duplex(64 * 1024);
	let mut stream = bound(io, Version::QMux02, 16 * 1024);
	peer.write_all(b"\x05hello\x05world").await.unwrap();

	let first = recv(&mut stream).await.unwrap();
	let second = recv(&mut stream).await.unwrap();
	assert_eq!(first, &b"hello"[..]);
	assert_eq!(second, &b"world"[..]);
	// Both point into the one read buffer rather than copies of it.
	assert_eq!(first.as_ptr().wrapping_add(6), second.as_ptr());
}

/// A record larger than the read buffer, allowed by a raised `max_record_size`, gets a buffer
/// of its own.
#[tokio::test]
async fn a_record_larger_than_the_read_buffer_arrives_whole() {
	let (io, mut peer) = tokio::io::duplex(1 << 20);
	let size = 3 * READ_BUFFER;
	let mut stream = bound(io, Version::QMux02, size as u64);
	let payload: Vec<u8> = (0..size).map(|i| i as u8).collect();
	let mut wire = Vec::new();
	VarInt::try_from(size).unwrap().encode(&mut wire);
	wire.extend_from_slice(&payload);
	// A small record first, so the large one starts partway into a buffer.
	peer.write_all(b"\x01x").await.unwrap();
	peer.write_all(&wire).await.unwrap();

	assert_eq!(recv(&mut stream).await.unwrap(), &b"x"[..]);
	assert_eq!(recv(&mut stream).await.unwrap(), payload);
}

/// A record declaring more than `max_record_size` is refused at its header, before any of
/// its payload is read.
#[tokio::test]
async fn a_record_past_the_maximum_is_refused_at_its_header() {
	let (io, mut peer) = tokio::io::duplex(64 * 1024);
	let mut stream = bound(io, Version::QMux01, 16382);
	peer.write_all(&[0x7f, 0xff]).await.unwrap();
	assert!(matches!(recv(&mut stream).await, Err(Error::FrameTooLarge)));
}

/// draft-00 §5.2 bounds the whole frame at `max_frame_size`, header included: a frame of
/// exactly that size parses, and one byte more is a frame-size violation rather than something
/// to read into memory.
#[tokio::test]
async fn a_draft_00_frame_is_bounded_whole() {
	// STREAM with LEN (0x0a), stream id 2, a 2-byte length, then the payload.
	let frame = |payload: usize| {
		let mut frame = vec![0x0a, 0x02];
		frame.extend_from_slice(&(0x4000 | payload as u16).to_be_bytes());
		frame.resize(frame.len() + payload, 0x5a);
		Bytes::from(frame)
	};
	let at_limit = frame(MAX_FRAME_SIZE - 4);
	assert_eq!(at_limit.len(), MAX_FRAME_SIZE);
	let over_limit = frame(MAX_FRAME_SIZE - 3);

	let (io, mut peer) = tokio::io::duplex(64 * 1024);
	let mut stream = bound(io, Version::QMux00, 0);
	peer.write_all(&at_limit).await.unwrap();
	assert_eq!(recv(&mut stream).await.unwrap(), at_limit);

	peer.write_all(&over_limit).await.unwrap();
	assert!(matches!(recv(&mut stream).await, Err(Error::FrameTooLarge)));
}

/// draft-00 frames a byte stream cannot delimit are refused: an unknown type, and STREAM
/// without its Length field.
#[tokio::test]
async fn an_undelimited_draft_00_frame_is_refused() {
	for wire in [&[0x02][..], &[0x08, 0x04]] {
		let (io, mut peer) = tokio::io::duplex(64);
		let mut stream = bound(io, Version::QMux00, 0);
		peer.write_all(wire).await.unwrap();
		assert!(matches!(recv(&mut stream).await, Err(Error::FrameEncoding)), "{wire:?}");
	}
}

/// Closing finishes our send half, then keeps reading until the peer finishes its own, since
/// closing a TCP socket with unread data can reset it and discard our close.
#[tokio::test(start_paused = true)]
async fn closing_drains_the_peer_until_it_finishes() {
	for &version in Version::ALL {
		let (io, mut peer) = tokio::io::duplex(64);
		let mut stream = bound(io, version, 16 * 1024);
		let mut close = std::pin::pin!(kio::wait(|waiter| stream.poll_close(waiter)));
		let mut extra = [0];
		tokio::select! {
			biased;
			result = &mut close => panic!("{version:?}: closed before the peer finished: {result:?}"),
			result = peer.read(&mut extra) => assert_eq!(result.unwrap(), 0),
		}

		// More than the pipe holds proves the stream is draining rather than merely open.
		let finish = async {
			peer.write_all(&[0; 1024]).await.expect("drained");
			peer.shutdown().await.expect("peer FIN");
		};
		let (result, ()) = tokio::join!(close, finish);
		result.expect("graceful close");
	}
}

/// A framing error ends the receive side, but closing still drains what the peer sends.
#[tokio::test(start_paused = true)]
async fn closing_after_a_framing_error_still_drains_the_peer() {
	let (io, mut peer) = tokio::io::duplex(64);
	let mut stream = bound(io, Version::QMux02, 16382);
	peer.write_all(&[0x7f, 0xff]).await.unwrap();
	assert!(matches!(recv(&mut stream).await, Err(Error::FrameTooLarge)));

	let mut close = std::pin::pin!(kio::wait(|waiter| stream.poll_close(waiter)));
	let mut extra = [0];
	tokio::select! {
		biased;
		result = &mut close => panic!("closed before the malformed peer finished: {result:?}"),
		result = peer.read(&mut extra) => assert_eq!(result.unwrap(), 0),
	}
	let finish = async {
		peer.write_all(&[0; 1024]).await.expect("drained");
		peer.shutdown().await.expect("peer FIN");
	};
	let (result, ()) = tokio::join!(close, finish);
	result.expect("graceful close");
}
