//! QMux record I/O over a reliable byte stream: TCP, TLS, or a Unix socket.

use std::io;
use std::pin::Pin;
use std::task::{Poll, ready};

use bytes::{Buf, Bytes, BytesMut};
use moq_quic::VarInt;
use tokio::io::{AsyncRead, AsyncWrite};

use super::Transport;
use crate::protocol::validate_protocol;
use crate::{Config, Driver, Error, MAX_FRAME_SIZE, Protocol, Session};

/// The size of each read buffer: one default record with its size field, or one draft-00
/// frame. Records are sliced out of it rather than copied, and the session counts every slice
/// as pinning a whole buffer of this size. A larger record gets a buffer of its own size.
const READ_BUFFER: usize = MAX_FRAME_SIZE;

/// The least room a read is worth a syscall for; a fuller buffer moves its partial record to
/// a fresh one first.
const MIN_READ: usize = 1024;

/// Queued output past which [`Transport::poll_ready`] flushes before taking more.
const WRITE_BUFFER: usize = 64 * 1024;

/// QMux record I/O over any reliable byte stream (`AsyncRead + AsyncWrite`).
///
/// Delimits records itself, so the session sees each one whole. Pass it to
/// [`Session::connect`] or [`Session::accept`] to run QMux over a transport the built-in
/// `tcp`, `tls`, and `ws` helpers don't cover, such as a pipe, an in-memory duplex, or a
/// custom tunnel:
///
/// ```no_run
/// # async fn f(stream: tokio::net::TcpStream) -> Result<(), qmux::Error> {
/// use qmux::transport::Stream;
/// use qmux::{Config, Session, Version};
///
/// let config = Config::new(Version::QMux01);
/// let (session, driver) = Session::connect(Stream::new(stream), config).await?;
/// tokio::spawn(driver);
/// # let _ = session; Ok(())
/// # }
/// ```
pub struct Stream<T> {
	io: T,
	socket_stats: Option<crate::SharedSocketStats>,
	framing: Framing,

	/// Bytes read but not yet handed out; records are split off its front.
	read: BytesMut,
	/// How many bytes `read` must hold to complete the record at its front, once known.
	need: usize,
	/// The peer finished its send half, or reading it failed.
	eof: bool,

	/// Records and their size fields, waiting for the socket.
	write: Vec<u8>,
	/// How much of `write` the socket already took.
	written: usize,
	/// Our send half is finished.
	shutdown: bool,
}

/// How records are delimited on the byte stream.
#[derive(Clone, Copy, Debug)]
enum Framing {
	/// Records behind a Size field, refusing one larger than this (draft-01 and later)
	Records(usize),
	/// Self-delimiting frames (draft-00)
	Frames,
}

impl Framing {
	fn new(config: &Config) -> Self {
		match config.version.uses_records() {
			true => Self::Records(usize::try_from(config.max_record_size).unwrap_or(usize::MAX)),
			false => Self::Frames,
		}
	}
}

impl<T: AsyncRead + AsyncWrite + Unpin> Stream<T> {
	/// Wrap a byte stream; session establishment binds its framing and starts I/O.
	pub fn new(io: T) -> Self {
		Self {
			io,
			socket_stats: None,
			framing: Framing::new(&Config::default()),
			read: BytesMut::new(),
			need: 0,
			eof: false,
			write: Vec::new(),
			written: 0,
			shutdown: false,
		}
	}

	/// Report statistics for the connection underneath this stream.
	///
	/// `Stream` is generic over the byte stream, so it can't discover the socket on its own;
	/// the builder that knows the concrete type attaches it. See `TcpStats` (Linux and macOS).
	pub fn with_socket_stats(mut self, stats: crate::SharedSocketStats) -> Self {
		self.socket_stats = Some(stats);
		self
	}

	/// Split the record at the front of the read buffer off, if it is all there.
	fn parse(&mut self) -> Result<Option<Bytes>, Error> {
		let header = match self.framing {
			Framing::Records(max) => peek_varint(&self.read)
				.map(|(size, prefix)| {
					let size = usize::try_from(size).map_err(|_| Error::FrameTooLarge)?;
					if size > max {
						return Err(Error::FrameTooLarge);
					}
					Ok((prefix + size, prefix))
				})
				.transpose()?,
			Framing::Frames => frame_len(&self.read)?.map(|len| (len, 0)),
		};
		let Some((total, prefix)) = header else {
			self.need = self.read.len() + 1;
			return Ok(None);
		};
		if self.read.len() < total {
			self.need = total;
			return Ok(None);
		}
		let mut record = self.read.split_to(total);
		record.advance(prefix);
		Ok(Some(record.freeze()))
	}

	/// Read more of the peer's bytes into the read buffer, setting `eof` once it finished.
	fn poll_fill(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		// What the record at the front still needs, which this buffer must have room for.
		let need = self.need.saturating_sub(self.read.len()).max(1);
		let want = need.max(MIN_READ);
		// Reuse the buffer in place once no record still points into it; otherwise move the
		// partial record to a fresh one, sized to the record when it is larger than usual.
		if self.read.capacity() - self.read.len() < want && !self.read.try_reclaim(want) {
			let mut fresh = BytesMut::with_capacity(READ_BUFFER.max(self.read.len() + need));
			fresh.extend_from_slice(&self.read);
			self.read = fresh;
		}
		let mut cx = waiter.context();
		match ready!(tokio_util::io::poll_read_buf(
			Pin::new(&mut self.io),
			&mut cx,
			&mut self.read
		)) {
			Ok(0) => self.eof = true,
			Ok(_) => {}
			Err(err) => {
				self.eof = true;
				return Poll::Ready(Err(err.into()));
			}
		}
		Poll::Ready(Ok(()))
	}
}

impl<T: AsyncRead + AsyncWrite + Unpin> Transport for Stream<T> {
	fn bind(&mut self, config: &Config) {
		self.framing = Framing::new(config);
	}

	fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		loop {
			if let Some(record) = self.parse()? {
				return Poll::Ready(Ok(record));
			}
			if self.eof {
				// The peer finished mid-record, or without the close frame that ends a session.
				return Poll::Ready(Err(io::Error::from(io::ErrorKind::UnexpectedEof).into()));
			}
			ready!(self.poll_fill(waiter))?;
		}
	}

	fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		if self.write.len() >= WRITE_BUFFER {
			ready!(self.poll_flush(waiter))?;
		}
		Poll::Ready(Ok(()))
	}

	fn send(&mut self, record: Bytes) -> Result<(), Error> {
		// Record-framed drafts travel inside size-prefixed records on byte streams. (Records
		// are implicit on WebSocket, where the message boundary delimits them.)
		if let Framing::Records(_) = self.framing {
			VarInt::try_from(record.len())
				.map_err(|_| Error::FrameTooLarge)?
				.encode(&mut self.write);
		}
		self.write.extend_from_slice(&record);
		Ok(())
	}

	fn poll_flush(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		let mut cx = waiter.context();
		while self.written < self.write.len() {
			let n = ready!(Pin::new(&mut self.io).poll_write(&mut cx, &self.write[self.written..]))?;
			if n == 0 {
				return Poll::Ready(Err(io::Error::from(io::ErrorKind::WriteZero).into()));
			}
			self.written += n;
		}
		self.write.clear();
		self.written = 0;
		ready!(Pin::new(&mut self.io).poll_flush(&mut cx))?;
		Poll::Ready(Ok(()))
	}

	fn poll_close(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>> {
		ready!(self.poll_flush(waiter))?;
		if !self.shutdown {
			ready!(Pin::new(&mut self.io).poll_shutdown(&mut waiter.context()))?;
			self.shutdown = true;
		}
		// Closing a TCP socket with unread peer data can reset it and discard our close, so
		// discard what the peer still sends until it finishes too.
		while !self.eof {
			self.read.clear();
			self.need = 0;
			ready!(self.poll_fill(waiter))?;
		}
		Poll::Ready(Ok(()))
	}

	fn socket_stats(&self) -> Option<crate::SharedSocketStats> {
		self.socket_stats.clone()
	}
}

/// The varint at the front of `buf` and its encoded length, or `None` while it is incomplete.
fn peek_varint(buf: &[u8]) -> Option<(u64, usize)> {
	let first = *buf.first()?;
	let len = 1usize << (first >> 6);
	let bytes = buf.get(..len)?;
	let mut raw = [0u8; 8];
	raw[8 - len..].copy_from_slice(bytes);
	raw[8 - len] &= 0x3f;
	Some((u64::from_be_bytes(raw), len))
}

/// A peer-sent length as `usize`, refused where it does not fit (32-bit targets)
fn peer_len(value: u64) -> Result<usize, Error> {
	usize::try_from(value).map_err(|_| Error::FrameTooLarge)
}

/// Walks a draft-00 frame header.
struct Header<'a> {
	buf: &'a [u8],
	pos: usize,
}

impl Header<'_> {
	/// The next varint, or `None` while the header is incomplete.
	fn varint(&mut self) -> Option<u64> {
		let (value, len) = peek_varint(&self.buf[self.pos..])?;
		self.pos += len;
		Some(value)
	}

	/// Skip `count` varints.
	fn skip(&mut self, count: usize) -> Option<()> {
		for _ in 0..count {
			self.varint()?;
		}
		Some(())
	}

	/// A length-prefixed body: the frame's whole length, refused past `max_frame_size`.
	///
	/// draft-00 §5.2 bounds the whole frame, header included. A byte stream cannot
	/// resynchronize past an oversized frame, so this is fatal, as the draft requires
	/// (FRAME_ENCODING_ERROR).
	fn body(&mut self) -> Result<Option<usize>, Error> {
		let Some(len) = self.varint() else {
			return Ok(None);
		};
		let total = self.pos.saturating_add(peer_len(len)?);
		if total > MAX_FRAME_SIZE {
			return Err(Error::FrameTooLarge);
		}
		Ok(Some(total))
	}
}

/// The whole length of the draft-00 frame at the front of `buf`, once its header is there.
fn frame_len(buf: &[u8]) -> Result<Option<usize>, Error> {
	let mut header = Header { buf, pos: 0 };
	let Some(frame_type) = header.varint() else {
		return Ok(None);
	};

	// STREAM frames: 0x08-0x0f
	if (0x08..=0x0f).contains(&frame_type) {
		let has_off = frame_type & 0x04 != 0;
		let has_len = frame_type & 0x02 != 0;
		// A byte stream cannot delimit a STREAM frame without its length.
		if !has_len {
			return Err(Error::FrameEncoding);
		}
		if header.skip(1 + usize::from(has_off)).is_none() {
			return Ok(None);
		}
		return header.body();
	}

	let fixed = match frame_type {
		// PADDING
		0x00 => 0,
		// RESET_STREAM: id, code, final size
		0x04 => 3,
		// STOP_SENDING: id, code
		0x05 => 2,
		// CONNECTION_CLOSE (0x1c) carries the Frame Type field; APPLICATION_CLOSE (0x1d) omits
		// it (RFC 9000 §19.19). Then the reason phrase.
		0x1c | 0x1d => {
			let fields = if frame_type == 0x1c { 2 } else { 1 };
			if header.skip(fields).is_none() {
				return Ok(None);
			}
			return header.body();
		}
		// MAX_DATA, MAX_STREAMS, DATA_BLOCKED, STREAMS_BLOCKED
		0x10 | 0x12 | 0x13 | 0x14 | 0x16 | 0x17 => 1,
		// MAX_STREAM_DATA, STREAM_DATA_BLOCKED: id, limit
		0x11 | 0x15 => 2,
		// QX_TRANSPORT_PARAMETERS
		0x3f5153300d0a0d0a => return header.body(),
		// Draft-00 has no datagrams or QX_PING, and an unknown type cannot be skipped on a byte
		// stream.
		_ => return Err(Error::FrameEncoding),
	};
	Ok(header.skip(fixed).map(|()| header.pos))
}

/// Wrap a byte stream in a [`Stream`] and start a session, validating any advertised protocol
/// names first. Used by the `tcp` and `uds` builders.
pub(crate) async fn build<T: AsyncRead + AsyncWrite + Unpin>(
	io: T,
	config: Config,
	is_server: bool,
	socket_stats: Option<crate::SharedSocketStats>,
) -> Result<(Session, Driver<Stream<T>>), Error> {
	if let Protocol::Negotiate(protocols) = &config.protocol {
		for protocol in protocols {
			validate_protocol(protocol)?;
		}
	}
	let mut transport = Stream::new(io);
	if let Some(stats) = socket_stats {
		transport = transport.with_socket_stats(stats);
	}
	match is_server {
		true => Session::accept(transport, config).await,
		false => Session::connect(transport, config).await,
	}
}

#[cfg(test)]
mod tests;

#[cfg(test)]
fn bound<T: AsyncRead + AsyncWrite + Unpin>(io: T, version: crate::Version, max_record_size: u64) -> Stream<T> {
	let mut stream = Stream::new(io);
	stream.bind(&Config {
		max_record_size,
		..Config::new(version)
	});
	stream
}
