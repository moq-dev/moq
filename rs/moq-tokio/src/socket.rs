//! Socket sessions as tokio I/O.
//!
//! [`TcpStream`] reads and writes a [`moq_net::socket::TcpStream`] through
//! [`AsyncRead`] and [`AsyncWrite`], with TCP's semantics: bytes arrive in order with
//! nothing lost, [`AsyncWriteExt::shutdown`](tokio::io::AsyncWriteExt::shutdown) is FIN,
//! and an error is a reset. So it drops into anything that forwards a TCP connection,
//! such as [`tokio::io::copy_bidirectional`].

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, ready};

use bytes::Bytes;
use moq_net::{frame, group};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// The most a write puts in one frame.
///
/// Each frame costs a header and a cache slot on every hop, so a write past this splits
/// into several. Through a local relay, 64 KiB to 256 KiB frames moved 300 MB fastest,
/// and 1 MiB frames were slower.
pub const MAX_FRAME: usize = 64 * 1024;

/// A `tcp` socket session as a byte stream: [`AsyncRead`] reads the peer's half, and
/// [`AsyncWrite`] writes this side's.
///
/// A read hands out a frame's bytes as they arrive, before the frame ends, and returns 0
/// once the peer shut down its half. A write waits until the peer's reader is at most a
/// window behind, so a slow reader slows the writer and nothing is lost, and becomes one
/// frame of at most [`MAX_FRAME`] bytes.
/// [`shutdown`](tokio::io::AsyncWriteExt::shutdown) finishes this side's half, which the
/// peer reads as the end, and reading on stays open.
///
/// Every failure is a reset of the whole session, read and written as an
/// [`io::ErrorKind::ConnectionReset`] carrying the [`moq_net::Error`] that caused it: the
/// peer's [`Self::abort`], a reader falling behind its window
/// ([`moq_net::Error::Evicted`]), or the session to the relay ending. Dropping the stream
/// resets it too, unless both halves already ended.
pub struct TcpStream {
	/// This side's half, until it is shut down.
	writer: Option<group::Producer>,
	reader: group::Consumer,
	/// The peer's frame being read.
	frame: Option<frame::Consumer>,
	/// What a read got of a chunk but had no room for.
	chunk: Bytes,
	/// Whether the peer's half ended.
	eof: bool,
	peer: String,
	session: String,
	read: kio::Park,
	write: kio::Park,
}

impl TcpStream {
	/// The peer endpoint's id.
	pub fn peer(&self) -> &str {
		&self.peer
	}

	/// The session's id, which the dialer chose. Keep it secret: anyone who knows it can
	/// read the session.
	pub fn session(&self) -> &str {
		&self.session
	}

	/// Reset the session with `err`, which the peer reads as its error, such as a
	/// [`moq_net::Error::App`] code.
	pub fn abort(mut self, err: moq_net::Error) {
		match self.writer.take() {
			Some(writer) => {
				let _ = writer.abort(err);
			}
			// Shut down already: the peer reads its end, and dropping the reader resets
			// the session.
			None => tracing::debug!(%err, "aborted a tcp session after its shutdown"),
		}
	}
}

impl From<moq_net::socket::TcpStream> for TcpStream {
	fn from(stream: moq_net::socket::TcpStream) -> Self {
		let (peer, session) = (stream.peer().to_owned(), stream.session().to_owned());
		Self::new(stream.producer, stream.consumer, peer, session)
	}
}

impl TcpStream {
	fn new(writer: group::Producer, reader: group::Consumer, peer: String, session: String) -> Self {
		Self {
			writer: Some(writer),
			reader,
			frame: None,
			chunk: Bytes::new(),
			eof: false,
			peer,
			session,
			read: kio::Park::default(),
			write: kio::Park::default(),
		}
	}
}

fn reset(err: moq_net::Error) -> io::Error {
	io::Error::new(io::ErrorKind::ConnectionReset, err)
}

impl AsyncRead for TcpStream {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		let this = self.get_mut();
		let waiter = this.read.hold(cx);
		loop {
			if buf.remaining() == 0 {
				return Poll::Ready(Ok(()));
			}
			if !this.chunk.is_empty() {
				let n = this.chunk.len().min(buf.remaining());
				buf.put_slice(&this.chunk.split_to(n));
				return Poll::Ready(Ok(()));
			}
			match &mut this.frame {
				Some(frame) => match ready!(frame.poll_read_chunk(waiter)).map_err(reset)? {
					Some(chunk) => this.chunk = chunk,
					None => this.frame = None,
				},
				None if this.eof => return Poll::Ready(Ok(())),
				None => match ready!(this.reader.poll_next_frame(waiter)).map_err(reset)? {
					Some(frame) => this.frame = Some(frame),
					None => this.eof = true,
				},
			}
		}
	}
}

impl AsyncWrite for TcpStream {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		let this = self.get_mut();
		let Some(writer) = &mut this.writer else {
			return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
		};
		if buf.is_empty() {
			return Poll::Ready(Ok(0));
		}
		let size = buf.len().min(MAX_FRAME);
		let info = frame::Info {
			size: size as u64,
			timestamp: None,
		};
		ready!(writer.poll_ready(this.write.hold(cx), &info));
		writer
			.write_frame(None, Bytes::copy_from_slice(&buf[..size]))
			.map_err(reset)?;
		Poll::Ready(Ok(size))
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		// A written frame is already on its way.
		Poll::Ready(Ok(()))
	}

	fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		if let Some(writer) = self.get_mut().writer.take() {
			writer.finish().map_err(reset)?;
		}
		Poll::Ready(Ok(()))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tokio::io::{AsyncReadExt, AsyncWriteExt};

	/// A group with its own broadcast and track, which outlive the test.
	fn group() -> group::Producer {
		let broadcast = moq_net::broadcast::Producer::new(Default::default());
		let track = broadcast.create_track("half", None).unwrap();
		let group = track.create_group(group::Info { sequence: 0 }).unwrap();
		std::mem::forget((broadcast, track));
		group
	}

	/// Two ends joined by a group each way, as a session's pump joins them.
	fn pair() -> (TcpStream, TcpStream) {
		let (ab, ba) = (group(), group());
		let (a_reads, b_reads) = (ba.consume(), ab.consume());
		(
			TcpStream::new(ab, a_reads, "b".into(), "s".into()),
			TcpStream::new(ba, b_reads, "a".into(), "s".into()),
		)
	}

	/// The `moq_net::Error` a reset carries.
	fn cause(err: &io::Error) -> &moq_net::Error {
		assert_eq!(err.kind(), io::ErrorKind::ConnectionReset, "{err}");
		err.get_ref()
			.and_then(|err| err.downcast_ref())
			.expect("a moq_net::Error")
	}

	fn poll_read(stream: &mut TcpStream, buf: &mut [u8]) -> Poll<io::Result<usize>> {
		let mut buf = ReadBuf::new(buf);
		let mut cx = Context::from_waker(std::task::Waker::noop());
		Pin::new(stream)
			.poll_read(&mut cx, &mut buf)
			.map_ok(|()| buf.filled().len())
	}

	#[kio::test]
	async fn a_frame_is_read_before_it_ends() {
		let writer = group();
		let mut b = TcpStream::new(group(), writer.consume(), "a".into(), "s".into());
		let mut frame = writer
			.create_frame(frame::Info {
				size: 10,
				timestamp: None,
			})
			.unwrap();
		frame.write(Bytes::from_static(b"hell")).unwrap();
		let mut buf = [0; 16];
		assert!(matches!(poll_read(&mut b, &mut buf), Poll::Ready(Ok(4))));
		assert_eq!(&buf[..4], b"hell");
		assert!(poll_read(&mut b, &mut buf).is_pending());
		frame.write(Bytes::from_static(b"o, you")).unwrap();
		frame.finish().unwrap().finish().unwrap();
		let mut rest = Vec::new();
		b.read_to_end(&mut rest).await.unwrap();
		assert_eq!(rest, b"o, you");
	}

	#[kio::test]
	async fn bytes_round_trip_and_large_writes_split() {
		let (mut a, mut b) = pair();
		let sent: Vec<u8> = (0..3 * MAX_FRAME + 17).map(|i| i as u8).collect();
		assert_eq!(a.write(&sent).await.unwrap(), MAX_FRAME);
		a.write_all(&sent[MAX_FRAME..]).await.unwrap();
		a.shutdown().await.unwrap();
		let mut got = Vec::new();
		b.read_to_end(&mut got).await.unwrap();
		assert_eq!(got, sent);
		assert_eq!(b.reader.frame_count(), 4);
	}

	/// Shutting down one half is FIN: the peer reads to the end, and its own half stays open
	/// both ways.
	#[kio::test]
	async fn shutdown_closes_one_half() {
		for first in [true, false] {
			let (a, b) = pair();
			let (mut closer, mut other) = if first { (a, b) } else { (b, a) };
			closer.write_all(b"bye").await.unwrap();
			closer.shutdown().await.unwrap();
			assert_eq!(closer.write(b"x").await.unwrap_err().kind(), io::ErrorKind::BrokenPipe);
			let mut got = Vec::new();
			other.read_to_end(&mut got).await.unwrap();
			assert_eq!(got, b"bye");

			other.write_all(b"still here").await.unwrap();
			other.shutdown().await.unwrap();
			let mut got = Vec::new();
			closer.read_to_end(&mut got).await.unwrap();
			assert_eq!(got, b"still here");
		}
	}

	/// An abort is a reset carrying its code, which like TCP's may discard bytes still in
	/// flight.
	#[kio::test]
	async fn abort_resets_with_its_code() {
		for first in [true, false] {
			let (a, b) = pair();
			let (mut aborter, mut other) = if first { (a, b) } else { (b, a) };
			aborter.write_all(b"partial").await.unwrap();
			aborter.abort(moq_net::Error::App(7));
			let err = other.read_to_end(&mut Vec::new()).await.unwrap_err();
			assert!(matches!(cause(&err), moq_net::Error::App(7)), "{err}");
		}
	}

	/// A reader that fell behind its window is reset, never handed a gap or an end.
	#[kio::test]
	async fn eviction_is_a_reset() {
		let (mut a, mut b) = pair();
		// What a session's pump does when the peer's half shed a frame its reader needed.
		a.writer.take().unwrap().abort(moq_net::Error::Evicted).unwrap();
		let err = b.read(&mut [0; 4]).await.unwrap_err();
		assert!(matches!(cause(&err), moq_net::Error::Evicted), "{err}");
	}
}
