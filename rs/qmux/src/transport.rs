use bytes::Bytes;

use crate::Error;

/// Abstracts message I/O over a reliable transport.
///
/// Each `send`/`recv` operates on a single complete message (frame).
/// For WebSocket, this maps to individual WS binary messages.
/// For TCP/TLS byte streams, the transport handles frame delimiting.
///
/// Use [`Stream`] for arbitrary [`AsyncRead`](tokio::io::AsyncRead) +
/// [`AsyncWrite`](tokio::io::AsyncWrite) byte streams. Implement this trait when
/// integrating a transport with its own message boundaries, framing, or
/// lifecycle that the built-in byte-stream and WebSocket adapters do not cover.
///
/// A transport splits into independently-owned send and receive halves so the
/// session can drive them on separate tasks: a write blocked on transport
/// backpressure must never stall reads (and vice versa). This decoupling is what
/// lets the session observe backpressure and shed unreliable datagrams instead
/// of buffering them behind a stalled socket.
pub trait Transport: Send + 'static {
	/// The independently-owned send half.
	type Writer: Writer;
	/// The independently-owned receive half.
	type Reader: Reader;

	/// Bind the session framing and split into send and receive halves.
	fn split(self, config: &crate::Config) -> (Self::Writer, Self::Reader);

	/// Statistics for the connection underneath, if it exposes any.
	///
	/// Read before the split, since the halves are handed to separate tasks. A
	/// transport that returns `None` leaves the session reporting only what it can
	/// measure itself, i.e. the `QX_PING` round trip.
	fn socket_stats(&self) -> Option<crate::SharedSocketStats> {
		None
	}

	/// Where the session's request target travels. Most transports carry no request URI,
	/// so the default is [`Address::Setup`](moq_net::transport::Address::Setup).
	fn address(&self) -> moq_net::transport::Address {
		moq_net::transport::Address::Setup
	}
}

/// The send half of a [`Transport`].
pub trait Writer: Send + 'static {
	/// Send a single complete message.
	fn send(&mut self, data: Bytes) -> impl std::future::Future<Output = Result<(), Error>> + Send;

	/// Queue a single complete message without flushing it. The session's writer
	/// feeds every frame that is already queued and then calls [`Writer::flush`]
	/// once, so a burst of small frames reaches the socket in one write instead of
	/// one write (and one TCP segment) each. The default sends immediately.
	fn feed(&mut self, data: Bytes) -> impl std::future::Future<Output = Result<(), Error>> + Send {
		self.send(data)
	}

	/// Write out everything queued by [`Writer::feed`]. The default has nothing to
	/// flush because the default `feed` already sent.
	fn flush(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send {
		std::future::ready(Ok(()))
	}

	/// Gracefully close the transport. Drop the [`Reader`] first so a byte stream also drains
	/// the peer until it finishes, which only the peer bounds, so bound or cancel the wait;
	/// otherwise this closes only the send half.
	fn close(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send;

	/// Perform any timer-driven background work and resolve once it's done. The
	/// session's writer loop selects on this alongside outbound frames, so a
	/// transport can piggy-back periodic maintenance (e.g. a WebSocket keep-alive
	/// Ping) on the same task that owns the send half. The default never
	/// resolves — transports with nothing to do (TCP, Unix sockets) use it as-is.
	fn maintain(&mut self) -> impl std::future::Future<Output = Result<(), Error>> + Send {
		std::future::pending()
	}
}

/// The receive half of a [`Transport`].
pub trait Reader: Send + 'static {
	/// Receive the next complete message.
	fn recv(&mut self) -> impl std::future::Future<Output = Result<Bytes, Error>> + Send;
}

// Stream: message I/O over a byte stream (TCP/TLS/Unix).
// Handles QMux frame delimiting to return complete frames as Bytes.
//
// Cancel safety: a dedicated reader task owns the read half and pushes complete
// frames into an `mpsc` channel. `recv()` is just `rx.recv().await`, which is
// cancel safe — if the future is dropped (e.g. a sibling `tokio::select!` branch
// wins), the buffered frame stays in the channel for the next call. The reader
// task itself never gets cancelled mid-parse, so the multi-step async reads in
// `recv_record`/`recv_qmux00_frame` are safe to keep as-is.
#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
mod stream_transport {
	use std::sync::Arc;

	use bytes::{BufMut, Bytes, BytesMut};
	use moq_quic::VarInt;
	use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};
	use tokio::sync::mpsc;
	use tokio::task::{AbortHandle, JoinHandle};

	use super::{Reader, Transport, Writer};
	use crate::{Error, MAX_FRAME_SIZE, Version};

	/// Bound on queued frames waiting for the session to drain them. Bytes the
	/// session hasn't picked up yet are also buffered in the OS receive window
	/// once this fills; the channel just gives a small slack so each `recv()`
	/// is a cheap hand-off rather than a syscall.
	const RECV_CHANNEL_CAPACITY: usize = 16;

	/// QMux message I/O over any reliable byte stream (`AsyncRead + AsyncWrite`).
	///
	/// Handles QMux frame/record delimiting so [`Session`](crate::Session) sees
	/// complete frames. Pair it with [`Session::connect`](crate::Session::connect)
	/// or [`Session::accept`](crate::Session::accept) to run QMux over a transport
	/// the built-in `tcp`/`tls`/`ws` helpers don't cover — a Unix socket, a pipe,
	/// an in-memory duplex, a custom tunnel, etc.:
	///
	/// ```no_run
	/// # async fn f(stream: tokio::net::TcpStream) -> Result<(), qmux::Error> {
	/// use qmux::transport::Stream;
	/// use qmux::{Config, Session, Version};
	///
	/// let config = Config::new(Version::QMux01);
	/// let transport = Stream::new(stream);
	/// let session = Session::connect(transport, config).await?;
	/// # let _ = session; Ok(())
	/// # }
	/// ```
	pub struct Stream<T> {
		stream: T,
		socket_stats: Option<crate::SharedSocketStats>,
	}

	/// The send half of a byte-stream [`Stream`].
	pub struct StreamWriter<T> {
		writer: BufWriter<tokio::io::WriteHalf<T>>,
		version: Version,
		reader_task: Option<JoinHandle<()>>,
		reader_lifetime: Arc<ReaderTask>,
	}

	/// The receive half of a byte-stream [`Stream`].
	pub struct StreamReader {
		rx: mpsc::Receiver<Result<Bytes, Error>>,
		_reader_lifetime: Arc<ReaderTask>,
	}

	// Either half can outlive the other; the last owner cancels the socket read.
	struct ReaderTask(AbortHandle);

	impl Drop for ReaderTask {
		fn drop(&mut self) {
			self.0.abort();
		}
	}

	impl<T: AsyncRead + AsyncWrite + Send + 'static> Stream<T> {
		/// Wrap a byte stream; session establishment binds its framing and starts I/O.
		pub fn new(stream: T) -> Self {
			Self {
				stream,
				socket_stats: None,
			}
		}

		/// Report statistics for the connection underneath this stream.
		///
		/// `Stream` is generic over the byte stream, so it can't discover the socket
		/// on its own; the builder that knows the concrete type attaches it. See
		/// `TcpStats` (Linux and macOS).
		pub fn with_socket_stats(mut self, stats: crate::SharedSocketStats) -> Self {
			self.socket_stats = Some(stats);
			self
		}
	}

	impl<T: AsyncRead + AsyncWrite + Send + 'static> Transport for Stream<T> {
		type Writer = StreamWriter<T>;
		type Reader = StreamReader;

		fn split(self, config: &crate::Config) -> (StreamWriter<T>, StreamReader) {
			let (read, write) = tokio::io::split(self.stream);
			let (tx, rx) = mpsc::channel(RECV_CHANNEL_CAPACITY);
			let reader_task = tokio::spawn(reader_loop(
				BufReader::new(read),
				config.version,
				usize::try_from(config.max_record_size).unwrap_or(usize::MAX),
				tx,
			));
			let reader_lifetime = Arc::new(ReaderTask(reader_task.abort_handle()));
			(
				StreamWriter {
					writer: BufWriter::new(write),
					version: config.version,
					reader_task: Some(reader_task),
					reader_lifetime: reader_lifetime.clone(),
				},
				StreamReader {
					rx,
					_reader_lifetime: reader_lifetime,
				},
			)
		}

		fn socket_stats(&self) -> Option<crate::SharedSocketStats> {
			self.socket_stats.clone()
		}
	}

	impl<T: AsyncWrite + Send + 'static> Writer for StreamWriter<T> {
		async fn send(&mut self, data: Bytes) -> Result<(), Error> {
			self.feed(data).await?;
			self.flush().await
		}

		async fn feed(&mut self, data: Bytes) -> Result<(), Error> {
			// Record-framed drafts (QMux01+) travel inside size-prefixed records
			// on byte streams. (Records are implicit on WebSocket, where the
			// message boundary delimits them.) Left in the BufWriter, which only
			// writes to the socket when full; `flush` sends the rest.
			if self.version.uses_records() {
				let mut size_buf = BytesMut::with_capacity(8);
				VarInt::try_from(data.len())
					.map_err(|_| Error::FrameTooLarge)?
					.encode(&mut size_buf);
				self.writer.write_all(&size_buf).await?;
			}
			self.writer.write_all(&data).await?;
			Ok(())
		}

		async fn flush(&mut self) -> Result<(), Error> {
			self.writer.flush().await?;
			Ok(())
		}

		async fn close(&mut self) -> Result<(), Error> {
			self.writer.shutdown().await?;
			// A retained receive half owns frame delivery and can leave the channel full.
			// Closing only the send half must neither wait for it nor discard its frames.
			if Arc::strong_count(&self.reader_lifetime) > 1 {
				return Ok(());
			}
			// Closing a TCP socket with unread peer data can reset it and discard our close.
			if let Some(task) = self.reader_task.as_mut() {
				let result = task.await;
				self.reader_task = None;
				result.map_err(std::io::Error::other)?;
			}
			Ok(())
		}
	}

	impl Reader for StreamReader {
		async fn recv(&mut self) -> Result<Bytes, Error> {
			// mpsc::Receiver::recv is cancel safe, so dropping this future never
			// loses a buffered frame. `None` means the reader task exited without
			// sending — treat as a clean close.
			self.rx.recv().await.unwrap_or(Err(Error::Closed))
		}
	}

	/// Reader task: pull complete frames off the wire and ship them through `tx`.
	/// After a parse error or receiver drop, drain the peer until EOF so the writer
	/// can close gracefully.
	async fn reader_loop<R: AsyncRead + Unpin>(
		mut reader: BufReader<R>,
		version: Version,
		our_max_record_size: usize,
		tx: mpsc::Sender<Result<Bytes, Error>>,
	) {
		loop {
			let result = match version.uses_records() {
				true => recv_record(&mut reader, our_max_record_size).await,
				false => recv_qmux00_frame(&mut reader).await,
			};
			let stop = result.is_err();
			if tx.send(result).await.is_err() || stop {
				drop(tx);
				// The session no longer reads records, but the peer can still be finishing
				// its send half. Discard without buffering; the writer owns cancellation.
				let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
				return;
			}
		}
	}

	/// Read a varint from the stream, returning the decoded value.
	/// If `buf` is provided, appends the raw bytes to it.
	async fn read_varint_into<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut BytesMut) -> Result<VarInt, Error> {
		let first = reader.read_u8().await?;
		buf.put_u8(first);

		let tag = first >> 6;
		let len = 1usize << tag;

		if len == 1 {
			return Ok(VarInt::from(first & 0x3f));
		}

		let start = buf.len();
		buf.resize(start + len - 1, 0);
		reader.read_exact(&mut buf[start..]).await?;

		let mut raw = [0u8; 8];
		raw[0] = first & 0x3f;
		raw[1..len].copy_from_slice(&buf[start..start + len - 1]);

		let value = match len {
			2 => u16::from_be_bytes([raw[0], raw[1]]) as u64,
			4 => u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64,
			8 => u64::from_be_bytes(raw),
			_ => unreachable!(),
		};

		// Two tag bits leave 62 for the value, which is exactly a varint's range.
		Ok(VarInt::from_u64(value).expect("62-bit value"))
	}

	/// Read a varint from the stream without collecting raw bytes.
	async fn read_varint<R: AsyncRead + Unpin>(reader: &mut R) -> Result<VarInt, Error> {
		let first = reader.read_u8().await?;
		let tag = first >> 6;
		let len = 1usize << tag;

		if len == 1 {
			return Ok(VarInt::from(first & 0x3f));
		}

		let mut raw = [0u8; 8];
		raw[0] = first & 0x3f;
		reader.read_exact(&mut raw[1..len]).await?;

		let value = match len {
			2 => u16::from_be_bytes([raw[0], raw[1]]) as u64,
			4 => u32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64,
			8 => u64::from_be_bytes(raw),
			_ => unreachable!(),
		};

		// Two tag bits leave 62 for the value, which is exactly a varint's range.
		Ok(VarInt::from_u64(value).expect("62-bit value"))
	}

	/// Read exactly `len` bytes, appending to buf.
	async fn read_bytes<R: AsyncRead + Unpin>(reader: &mut R, len: usize, buf: &mut BytesMut) -> Result<(), Error> {
		let start = buf.len();
		buf.resize(start + len, 0);
		reader.read_exact(&mut buf[start..]).await?;
		Ok(())
	}

	/// A peer-sent length as `usize`, refused where it does not fit (32-bit targets)
	fn peer_len(value: VarInt) -> Result<usize, Error> {
		usize::try_from(value.into_inner()).map_err(|_| Error::FrameTooLarge)
	}

	/// Read one QMux Record from the byte stream (draft-01).
	/// Returns the record payload (frames concatenated).
	async fn recv_record<R: AsyncRead + Unpin>(reader: &mut R, our_max_record_size: usize) -> Result<Bytes, Error> {
		let size = peer_len(read_varint(reader).await?)?;
		if size > our_max_record_size {
			return Err(Error::FrameTooLarge);
		}
		let mut buf = BytesMut::zeroed(size);
		reader.read_exact(&mut buf).await?;
		Ok(buf.freeze())
	}

	/// Read one complete QMux frame from the byte stream (draft-00), returning raw bytes.
	async fn recv_qmux00_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Bytes, Error> {
		let mut buf = BytesMut::new();
		let frame_type = read_varint_into(reader, &mut buf).await?.into_inner();

		// STREAM frames: 0x08-0x0f
		if (0x08..=0x0f).contains(&frame_type) {
			let has_off = frame_type & 0x04 != 0;
			let has_len = frame_type & 0x02 != 0;

			read_varint_into(reader, &mut buf).await?; // stream id

			if has_off {
				read_varint_into(reader, &mut buf).await?; // offset
			}

			if has_len {
				let len = peer_len(read_varint_into(reader, &mut buf).await?)?;
				// draft-00 §5.2: `max_frame_size` bounds the whole frame, so the
				// header bytes read so far count against it. A byte stream cannot
				// resynchronize past an oversized frame, so this stays fatal — as
				// the draft requires (FRAME_ENCODING_ERROR).
				if buf.len().saturating_add(len) > MAX_FRAME_SIZE {
					return Err(Error::FrameTooLarge);
				}
				read_bytes(reader, len, &mut buf).await?;
			} else {
				// A byte stream cannot delimit a STREAM frame without its length.
				return Err(Error::FrameEncoding);
			}

			return Ok(buf.freeze());
		}

		match frame_type {
			// PADDING
			0x00 => {}
			// RESET_STREAM
			0x04 => {
				read_varint_into(reader, &mut buf).await?; // id
				read_varint_into(reader, &mut buf).await?; // code
				read_varint_into(reader, &mut buf).await?; // final_size
			}
			// STOP_SENDING
			0x05 => {
				read_varint_into(reader, &mut buf).await?; // id
				read_varint_into(reader, &mut buf).await?; // code
			}
			// CONNECTION_CLOSE (0x1c) carries the Frame Type field; APPLICATION_CLOSE
			// (0x1d) omits it (RFC 9000 §19.19).
			0x1c | 0x1d => {
				read_varint_into(reader, &mut buf).await?; // code
				if frame_type == 0x1c {
					read_varint_into(reader, &mut buf).await?; // frame_type
				}
				let reason_len = peer_len(read_varint_into(reader, &mut buf).await?)?;
				if buf.len().saturating_add(reason_len) > MAX_FRAME_SIZE {
					return Err(Error::FrameTooLarge);
				}
				read_bytes(reader, reason_len, &mut buf).await?;
			}
			// MAX_DATA
			0x10 => {
				read_varint_into(reader, &mut buf).await?;
			}
			// MAX_STREAM_DATA
			0x11 => {
				read_varint_into(reader, &mut buf).await?; // id
				read_varint_into(reader, &mut buf).await?; // max
			}
			// MAX_STREAMS (bidi/uni)
			0x12 | 0x13 => {
				read_varint_into(reader, &mut buf).await?;
			}
			// DATA_BLOCKED
			0x14 => {
				read_varint_into(reader, &mut buf).await?;
			}
			// STREAM_DATA_BLOCKED
			0x15 => {
				read_varint_into(reader, &mut buf).await?; // id
				read_varint_into(reader, &mut buf).await?; // limit
			}
			// STREAMS_BLOCKED (bidi/uni)
			0x16 | 0x17 => {
				read_varint_into(reader, &mut buf).await?;
			}
			// QX_TRANSPORT_PARAMETERS
			0x3f5153300d0a0d0a => {
				let len = peer_len(read_varint_into(reader, &mut buf).await?)?;
				if buf.len().saturating_add(len) > MAX_FRAME_SIZE {
					return Err(Error::FrameTooLarge);
				}
				read_bytes(reader, len, &mut buf).await?;
			}
			// Draft-00 has no datagrams or QX_PING, and an unknown type cannot be skipped
			// on a byte stream.
			_ => return Err(Error::FrameEncoding),
		}

		Ok(buf.freeze())
	}

	#[cfg(test)]
	mod tests {
		use super::*;
		use crate::transport::{Reader, Transport};
		use tokio::io::AsyncWriteExt;

		#[test]
		fn wrapping_a_stream_does_not_start_io() {
			let (io, _peer) = tokio::io::duplex(64);
			let _transport = Stream::new(io);
		}

		#[tokio::test(start_paused = true)]
		async fn graceful_close_drains_the_peer_after_our_send_half_ends() {
			for &version in Version::ALL {
				let (stream, mut peer) = tokio::io::duplex(64);
				let (mut writer, reader) = Stream::new(stream).split(&crate::Config {
					max_record_size: 16 * 1024,
					..crate::Config::new(version)
				});
				drop(reader);

				let mut close = Box::pin(writer.close());
				let mut extra = [0];
				tokio::select! {
					biased;
					result = &mut close => panic!("{version:?}: closed before the peer finished: {result:?}"),
					result = peer.read(&mut extra) => assert_eq!(result.unwrap(), 0),
				}

				// The peer can still have data in flight after it reads our FIN. More than the
				// pipe holds proves the receiver is draining it rather than merely staying alive.
				peer.write_all(&[0; 1024]).await.expect("drain peer data");
				peer.shutdown().await.expect("peer FIN");
				close.as_mut().await.expect("graceful close");
				drop(close);
				writer.close().await.expect("already closed");
			}
		}

		#[tokio::test(start_paused = true)]
		async fn closing_the_send_half_preserves_a_full_receive_channel() {
			for &version in Version::ALL {
				let (stream, mut peer) = tokio::io::duplex(1);
				let (mut writer, mut reader) = Stream::new(stream).split(&crate::Config {
					max_record_size: 16 * 1024,
					..crate::Config::new(version)
				});
				let (frame, payload): (&[u8], &[u8]) = if version.uses_records() {
					(&[1, 42], &[42])
				} else {
					(&[0], &[0]) // PADDING
				};
				// The one-byte pipe cannot accept the last frame until all preceding
				// frames have filled the channel. The receive half stays unpolled.
				for _ in 0..=RECV_CHANNEL_CAPACITY {
					peer.write_all(frame).await.expect("peer frame");
				}
				peer.shutdown().await.expect("peer FIN");
				assert_eq!(reader.rx.len(), RECV_CHANNEL_CAPACITY);

				tokio::select! {
					biased;
					result = writer.close() => result.expect("send half closed"),
					_ = std::future::ready(()) => panic!("{version:?}: send close waited for the retained receive half"),
				}
				let mut extra = [0];
				assert_eq!(peer.read(&mut extra).await.unwrap(), 0);
				drop(writer);
				for _ in 0..=RECV_CHANNEL_CAPACITY {
					assert_eq!(reader.recv().await.expect("preserved peer frame"), payload);
				}
			}
		}

		#[tokio::test(start_paused = true)]
		async fn a_framing_error_still_drains_peer_data_during_close() {
			let (stream, mut peer) = tokio::io::duplex(64);
			let (mut writer, mut reader) = Stream::new(stream).split(&crate::Config {
				max_record_size: 4,
				..crate::Config::new(Version::QMux02)
			});
			peer.write_all(&[5]).await.expect("oversized record header");
			assert!(matches!(reader.recv().await, Err(Error::FrameTooLarge)));
			drop(reader);

			let close = writer.close();
			tokio::pin!(close);
			let mut extra = [0];
			tokio::select! {
				biased;
				result = &mut close => panic!("closed before the malformed peer finished: {result:?}"),
				result = peer.read(&mut extra) => assert_eq!(result.unwrap(), 0),
			}
			peer.write_all(&[0; 1024]).await.expect("drain malformed peer data");
			peer.shutdown().await.expect("peer FIN");
			close.await.expect("graceful close");
		}

		#[tokio::test(start_paused = true)]
		async fn cancelling_graceful_close_releases_the_receive_half() {
			let (stream, mut peer) = tokio::io::duplex(64);
			let (mut writer, reader) = Stream::new(stream).split(&crate::Config {
				max_record_size: 16 * 1024,
				..crate::Config::new(Version::QMux02)
			});
			drop(reader);

			let mut close = Box::pin(writer.close());
			let mut extra = [0];
			tokio::select! {
				biased;
				result = &mut close => panic!("closed before the peer finished: {result:?}"),
				result = peer.read(&mut extra) => assert_eq!(result.unwrap(), 0),
			}
			drop(close);
			drop(writer);

			// A detached drain would let this write finish, or park it once the pipe fills.
			peer.write_all(&[0; 1024]).await.expect_err("receive half released");
		}

		#[tokio::test(start_paused = true)]
		async fn dropping_the_send_half_keeps_the_receive_half_usable() {
			let (stream, mut peer) = tokio::io::duplex(64);
			let (writer, mut reader) = Stream::new(stream).split(&crate::Config {
				max_record_size: 16 * 1024,
				..crate::Config::new(Version::QMux02)
			});
			drop(writer);
			peer.write_all(&[1, 42]).await.expect("peer record");
			assert_eq!(reader.recv().await.expect("receive half remains open"), &[42][..]);
		}

		// Drip a frame in one byte at a time, racing each `recv` against an
		// immediate yield so the recv future is dropped every iteration. With
		// a cancel-safe `recv`, the final call must still return the whole
		// frame intact.
		#[tokio::test]
		async fn recv_is_cancel_safe_across_partial_writes() {
			let (client, mut server) = tokio::io::duplex(64 * 1024);
			let (_writer, mut transport) = Stream::new(client).split(&crate::Config {
				max_record_size: 16 * 1024,
				..crate::Config::new(Version::QMux00)
			});

			// STREAM frame, type 0x0a (len bit set), id=4, length=5, payload="hello".
			let mut frame = Vec::new();
			frame.push(0x0a);
			frame.push(0x04);
			frame.push(0x05);
			frame.extend_from_slice(b"hello");

			for chunk in frame.chunks(1).take(frame.len() - 1) {
				server.write_all(chunk).await.unwrap();
				server.flush().await.unwrap();
				tokio::select! {
					_ = transport.recv() => panic!("recv completed with a partial frame"),
					_ = tokio::task::yield_now() => {}
				}
			}

			server.write_all(&frame[frame.len() - 1..]).await.unwrap();
			server.flush().await.unwrap();

			let got = transport.recv().await.expect("frame should decode");
			assert_eq!(&got[..], frame.as_slice());
		}

		#[tokio::test]
		async fn recv_qmux01_record_is_cancel_safe() {
			let (client, mut server) = tokio::io::duplex(64 * 1024);
			let (_writer, mut transport) = Stream::new(client).split(&crate::Config {
				max_record_size: 16 * 1024,
				..crate::Config::new(Version::QMux01)
			});

			// 1-byte varint length (0x08) followed by 8 bytes of payload.
			let mut record = vec![0x08];
			record.extend_from_slice(b"abcdefgh");

			for chunk in record.chunks(1).take(record.len() - 1) {
				server.write_all(chunk).await.unwrap();
				server.flush().await.unwrap();
				tokio::select! {
					_ = transport.recv() => panic!("recv completed with a partial record"),
					_ = tokio::task::yield_now() => {}
				}
			}

			server.write_all(&record[record.len() - 1..]).await.unwrap();
			server.flush().await.unwrap();

			let got = transport.recv().await.expect("record should decode");
			assert_eq!(&got[..], b"abcdefgh");
		}

		// Two frames arrive in a single write. Each recv() must return one
		// complete frame, in order. Exercises the channel queue + the reader
		// task looping on a buffer that still has bytes after parsing.
		#[tokio::test]
		async fn recv_returns_consecutive_frames_in_order() {
			let (client, mut server) = tokio::io::duplex(64 * 1024);
			let (_writer, mut transport) = Stream::new(client).split(&crate::Config {
				max_record_size: 16 * 1024,
				..crate::Config::new(Version::QMux00)
			});

			// Two STREAM frames (type 0x0a) for stream ids 4 and 8.
			let frame_a: Vec<u8> = [0x0a, 0x04, 0x05].into_iter().chain(*b"hello").collect();
			let frame_b: Vec<u8> = [0x0a, 0x08, 0x05].into_iter().chain(*b"world").collect();
			let mut combined = frame_a.clone();
			combined.extend_from_slice(&frame_b);

			server.write_all(&combined).await.unwrap();
			server.flush().await.unwrap();

			let got_a = transport.recv().await.expect("first frame should decode");
			let got_b = transport.recv().await.expect("second frame should decode");
			assert_eq!(&got_a[..], frame_a.as_slice());
			assert_eq!(&got_b[..], frame_b.as_slice());
		}

		// Reader task hits a parse error: `recv()` returns it, and the next
		// `recv()` returns Error::Closed since the task has exited.
		#[tokio::test]
		async fn recv_propagates_parse_error_then_closes() {
			let (client, mut server) = tokio::io::duplex(64 * 1024);
			let (_writer, mut transport) = Stream::new(client).split(&crate::Config {
				max_record_size: 16 * 1024,
				..crate::Config::new(Version::QMux00)
			});

			// Frame type 0x02 isn't a recognized QMux00 frame type.
			server.write_all(&[0x02]).await.unwrap();
			server.flush().await.unwrap();

			let err = transport.recv().await.expect_err("parse error expected");
			assert!(matches!(err, Error::FrameEncoding), "got {err:?}");

			// Task has exited after sending the error; subsequent recv sees the
			// closed channel and reports Error::Closed.
			let err = transport.recv().await.expect_err("closed expected");
			assert!(matches!(err, Error::Closed), "got {err:?}");
		}

		// A record whose declared size exceeds `our_max_record_size` is
		// rejected with FrameTooLarge before any payload is consumed.
		#[tokio::test]
		async fn recv_record_exceeding_max_returns_frame_too_large() {
			let (client, mut server) = tokio::io::duplex(64 * 1024);
			let (_writer, mut transport) = Stream::new(client).split(&crate::Config {
				max_record_size: 4,
				..crate::Config::new(Version::QMux01)
			});

			// 1-byte varint length = 5, which exceeds the configured max of 4.
			server.write_all(&[0x05]).await.unwrap();
			server.flush().await.unwrap();

			let err = transport.recv().await.expect_err("FrameTooLarge expected");
			assert!(matches!(err, Error::FrameTooLarge), "got {err:?}");
		}

		/// draft-00 §5.2 bounds the whole frame at `max_frame_size`, header
		/// included: a frame of exactly that size parses, and one byte more is a
		/// frame-size violation rather than something to read into memory.
		#[tokio::test]
		async fn recv_qmux00_bounds_the_whole_frame() {
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
			assert_eq!(over_limit.len(), MAX_FRAME_SIZE + 1);

			let (client, mut server) = tokio::io::duplex(64 * 1024);
			let (_writer, mut transport) = Stream::new(client).split(&crate::Config {
				max_record_size: 0,
				..crate::Config::new(Version::QMux00)
			});

			server.write_all(&at_limit).await.unwrap();
			server.flush().await.unwrap();
			assert_eq!(transport.recv().await.unwrap(), at_limit);

			server.write_all(&over_limit).await.unwrap();
			server.flush().await.unwrap();
			let err = transport.recv().await.expect_err("FrameTooLarge expected");
			assert!(matches!(err, Error::FrameTooLarge), "got {err:?}");
		}
	}
}

#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
pub use stream_transport::{Stream, StreamReader, StreamWriter};

// Shared plumbing for the byte-stream transports (TCP, Unix sockets).
#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
mod stream_session {
	use tokio::io::{AsyncRead, AsyncWrite};

	use super::Stream;
	use crate::protocol::validate_protocol;
	use crate::{Config, Error, Protocol, Session};

	/// Wrap a byte stream in a [`Stream`] and start a session, validating any
	/// advertised protocol names first. Used by the `tcp`/`uds` builders.
	pub(crate) async fn build<T: AsyncRead + AsyncWrite + Send + 'static>(
		stream: T,
		config: Config,
		is_server: bool,
		socket_stats: Option<crate::SharedSocketStats>,
	) -> Result<Session, Error> {
		if let Protocol::Negotiate(protocols) = &config.protocol {
			for protocol in protocols {
				validate_protocol(protocol)?;
			}
		}
		let mut transport = Stream::new(stream);
		if let Some(stats) = socket_stats {
			transport = transport.with_socket_stats(stats);
		}
		if is_server {
			Session::accept(transport, config).await
		} else {
			Session::connect(transport, config).await
		}
	}
}

#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
pub(crate) use stream_session::build as build_stream_session;

// WsTransport: message I/O over WebSocket.
#[cfg(feature = "ws")]
mod ws_transport {
	use std::time::Duration;

	use bytes::Bytes;
	use futures::stream::SplitSink;
	use kio::clock::{Interval, MissedTickBehavior, Sleep};
	use tokio::sync::mpsc;
	use tokio::task::JoinHandle;
	use tokio_tungstenite::tungstenite;

	use super::{Reader, Transport, Writer};
	use crate::ws::KeepAlive;
	use crate::{Error, MAX_FRAME_SIZE};

	type Message = tungstenite::Message;

	/// Bound on WS frames the pump task reads ahead of the session. Small: the pump
	/// only stages already-flow-control-covered frames; a full channel makes the
	/// pump park on delivery, which is exactly the backpressure signal the deadline
	/// logic keys off. Mirrors the byte-stream reader's `RECV_CHANNEL_CAPACITY`.
	const WS_RECV_CHANNEL_CAPACITY: usize = 16;

	/// The combined `Stream + Sink` bound every WebSocket half requires.
	pub(crate) trait WsStream:
		futures::Stream<Item = Result<Message, tungstenite::Error>>
		+ futures::Sink<Message, Error = tungstenite::Error>
		+ Unpin
		+ Send
		+ 'static
	{
	}
	impl<T> WsStream for T where
		T: futures::Stream<Item = Result<Message, tungstenite::Error>>
			+ futures::Sink<Message, Error = tungstenite::Error>
			+ Unpin
			+ Send
			+ 'static
	{
	}

	pub(crate) struct WsTransport<T> {
		ws: T,
		keep_alive: Option<KeepAlive>,
		socket_stats: Option<crate::SharedSocketStats>,
		address: moq_net::transport::Address,
	}

	impl<T> WsTransport<T> {
		pub fn new(ws: T, address: moq_net::transport::Address) -> Self {
			Self {
				ws,
				keep_alive: None,
				socket_stats: None,
				address,
			}
		}

		/// Refuses a keep-alive that would close a healthy idle peer: a zero interval, or a
		/// timeout no longer than the interval, which expires before the first Pong.
		pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Result<Self, Error> {
			if keep_alive.interval.is_zero() {
				return Err(Error::InvalidConfig("zero keep-alive interval"));
			}
			if keep_alive.timeout <= keep_alive.interval {
				return Err(Error::InvalidConfig("keep-alive timeout not above its interval"));
			}
			self.keep_alive = Some(keep_alive);
			Ok(self)
		}

		pub fn with_socket_stats(mut self, stats: crate::SharedSocketStats) -> Self {
			self.socket_stats = Some(stats);
			self
		}
	}

	/// Writer-side keep-alive: emit a Ping every `interval`.
	struct PingState {
		// Fires on each interval; the writer sends a Ping when it does.
		interval: Interval,
	}

	impl PingState {
		fn new(config: KeepAlive) -> Self {
			// The first tick is one interval out, so we don't ping on connect.
			let mut interval = kio::clock::interval_at(kio::clock::now() + config.interval, config.interval);
			// Skip catch-up bursts after a long pause; we just want one Ping per tick.
			interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
			Self { interval }
		}
	}

	/// Reader-side keep-alive: close the session if no frame arrives within `timeout`.
	struct DeadlineState {
		// Resets every time we receive a frame. If it elapses, the peer is gone.
		deadline: Sleep,
		timeout: Duration,
	}

	impl DeadlineState {
		fn new(config: KeepAlive) -> Self {
			let timeout = config.timeout;
			Self {
				deadline: kio::clock::sleep(timeout),
				timeout,
			}
		}

		fn observe_recv(&mut self) {
			self.deadline.reset(kio::clock::now() + self.timeout);
		}
	}

	/// The send half of a [`WsTransport`]: owns the sink and drives keep-alive Pings.
	pub(crate) struct WsWriter<T: WsStream> {
		sink: SplitSink<T, Message>,
		ping: Option<PingState>,
	}

	/// The receive half of a [`WsTransport`].
	///
	/// A dedicated pump task owns the socket and the idle deadline; `recv` is just a
	/// hand-off from its channel. Reading on its own task is what makes the keep-alive
	/// deadline robust: it advances (and resets on the peer's Pong replies) regardless
	/// of where the *session* is parked — in particular while the session is wedged
	/// handing a stream to a slow `accept_*`, which would otherwise starve a deadline
	/// polled inline in `recv` and fire a spurious timeout. Mirrors [`StreamReader`].
	pub(crate) struct WsReader {
		rx: mpsc::Receiver<Result<Bytes, Error>>,
		/// Aborted on drop so the pump can't outlive the transport.
		pump: JoinHandle<()>,
	}

	impl Drop for WsReader {
		fn drop(&mut self) {
			self.pump.abort();
		}
	}

	impl<T: WsStream> Transport for WsTransport<T> {
		type Writer = WsWriter<T>;
		type Reader = WsReader;

		fn socket_stats(&self) -> Option<crate::SharedSocketStats> {
			self.socket_stats.clone()
		}

		fn address(&self) -> moq_net::transport::Address {
			self.address
		}

		fn split(self, config: &crate::Config) -> (WsWriter<T>, WsReader) {
			use futures::StreamExt;
			// BiLock-backed halves: the sink and stream can be polled concurrently
			// on separate tasks, briefly serializing on the shared socket.
			let (sink, stream) = self.ws.split();
			let (ping, deadline) = match self.keep_alive {
				Some(ka) => (Some(PingState::new(ka)), Some(DeadlineState::new(ka))),
				None => (None, None),
			};
			let (tx, rx) = mpsc::channel(WS_RECV_CHANNEL_CAPACITY);
			let recv_limit = if config.version.uses_records() {
				usize::try_from(config.max_record_size).unwrap_or(usize::MAX)
			} else {
				MAX_FRAME_SIZE
			};
			let pump = tokio::spawn(ws_pump(stream, deadline, recv_limit, tx));
			(WsWriter { sink, ping }, WsReader { rx, pump })
		}
	}

	impl<T: WsStream> Writer for WsWriter<T> {
		async fn send(&mut self, data: Bytes) -> Result<(), Error> {
			use futures::SinkExt;
			self.sink.send(Message::Binary(data)).await.map_err(|_| Error::Closed)?;
			Ok(())
		}

		async fn feed(&mut self, data: Bytes) -> Result<(), Error> {
			use futures::SinkExt;
			// Still one WebSocket message per frame (the message boundary is the
			// record boundary), but tungstenite only appends it to its write
			// buffer. The socket write happens on `flush`, or early once the
			// buffer passes `write_buffer_size` (128 KiB by default).
			self.sink.feed(Message::Binary(data)).await.map_err(|_| Error::Closed)?;
			Ok(())
		}

		async fn flush(&mut self) -> Result<(), Error> {
			use futures::SinkExt;
			self.sink.flush().await.map_err(|_| Error::Closed)?;
			Ok(())
		}

		async fn close(&mut self) -> Result<(), Error> {
			use futures::SinkExt;
			self.sink.close().await.map_err(|_| Error::Closed)?;
			Ok(())
		}

		async fn maintain(&mut self) -> Result<(), Error> {
			use futures::SinkExt;
			match &mut self.ping {
				Some(ping) => {
					// Wait for the next interval, then send one keep-alive Ping. The
					// session's writer loop re-invokes this each time it resolves, so
					// pings keep flowing without a dedicated task. tungstenite's
					// auto-queued Pong replies (from the reader) also flush here.
					ping.interval.tick().await;
					self.sink
						.send(Message::Ping(Bytes::new()))
						.await
						.map_err(|_| Error::Closed)?;
					Ok(())
				}
				// No keep-alive configured: never resolves, so the writer loop's
				// select simply ignores this branch.
				None => std::future::pending().await,
			}
		}
	}

	impl Reader for WsReader {
		async fn recv(&mut self) -> Result<Bytes, Error> {
			// The pump task pushes complete Binary frames (or a terminal error)
			// here; `None` means it exited without sending — treat as a clean close.
			self.rx.recv().await.unwrap_or(Err(Error::Closed))
		}
	}

	/// Pump task: read WS messages off the socket independently of the session,
	/// resetting the keep-alive deadline on every message (including the peer's Pong
	/// replies) and shipping Binary frames to the session over `tx`. On a timeout,
	/// clean close, or socket error it sends a terminal `Err` and exits; if `tx` is
	/// closed (the transport was dropped) it exits silently.
	///
	/// Why a task rather than an inline deadline in `recv`: the deadline must keep
	/// advancing even while the session is parked (e.g. handing a stream to a slow
	/// `accept_*`), and the peer's liveness must not be judged while we're merely
	/// backpressured. So the deadline is reset both when a message arrives *and*
	/// after each delivery to the session — a long delivery park (session
	/// backpressure) therefore isn't charged against the peer.
	async fn ws_pump<S>(
		mut stream: S,
		mut deadline: Option<DeadlineState>,
		recv_limit: usize,
		tx: mpsc::Sender<Result<Bytes, Error>>,
	) where
		S: futures::Stream<Item = Result<Message, tungstenite::Error>> + Unpin + Send + 'static,
	{
		use futures::StreamExt;

		loop {
			// Prefer a real message over the deadline (`biased`): if both are ready,
			// the peer is alive, so don't spuriously time out.
			let message = match &mut deadline {
				Some(d) => tokio::select! {
					biased;
					msg = stream.next() => msg,
					_ = &mut d.deadline => {
						tracing::debug!("websocket keep_alive timeout");
						let _ = tx.send(Err(Error::Closed)).await;
						return;
					}
				},
				None => stream.next().await,
			};

			// Any read — data or control — proves the peer is alive; reset the deadline.
			if let Some(d) = deadline.as_mut() {
				d.observe_recv();
			}

			let message = match message {
				Some(Ok(message)) => message,
				// The socket errored mid-stream: forward the specific WebSocket
				// error (matching `StreamReader`, and preserving what the old inline
				// `recv()` surfaced via `?`) rather than flattening it to a generic
				// close, so callers can still distinguish e.g. a protocol fault.
				Some(Err(err)) => {
					let _ = tx.send(Err(err.into())).await;
					return;
				}
				// Clean end of stream.
				None => {
					let _ = tx.send(Err(Error::Closed)).await;
					return;
				}
			};

			match message {
				// The binding makes an empty message a no-op, unlike an empty record
				Message::Binary(data) if data.is_empty() => continue,
				Message::Binary(data) => {
					if data.len() > recv_limit {
						// Release the oversized allocation before waiting for room to
						// report the terminal error. In particular, never let it enter
						// the bounded record queue while the session is backpressured.
						drop(data);
						let _ = tx.send(Err(Error::FrameTooLarge)).await;
						return;
					}
					if tx.send(Ok(data)).await.is_err() {
						return; // session gone
					}
					// Delivered. Reset the deadline again so a long delivery park
					// (the session backpressured on `accept_*`) isn't charged
					// against the peer — we only judge liveness while actively
					// reading, not while wedged handing data to the app.
					if let Some(d) = deadline.as_mut() {
						d.observe_recv();
					}
				}
				Message::Close(_) => {
					let _ = tx.send(Err(Error::Closed)).await;
					return;
				}
				// QMux rides binary messages only; text means the peer speaks something else
				Message::Text(_) => {
					let _ = tx.send(Err(Error::ProtocolViolation)).await;
					return;
				}
				Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {
					// tungstenite auto-queues a Pong reply when it reads a Ping; the
					// writer half flushes it on its next send/ping. The pump owns no
					// sink, so there's nothing to reply with here.
					continue;
				}
			}
		}
	}

	#[cfg(test)]
	mod tests {
		use std::time::Duration;

		use bytes::Bytes;
		use tokio::sync::mpsc;

		use super::{DeadlineState, Message, ws_pump};
		use crate::Error;
		use crate::ws::KeepAlive;

		type WsResult = Result<Message, tokio_tungstenite::tungstenite::Error>;

		/// A `Stream` of WS messages backed by a `futures` channel. Holding the
		/// returned sender keeps the stream "open but silent" (`next()` pends) when
		/// empty; dropping it ends the stream. Standing in for a live peer.
		fn mock_stream() -> (
			futures::channel::mpsc::UnboundedSender<WsResult>,
			futures::channel::mpsc::UnboundedReceiver<WsResult>,
		) {
			futures::channel::mpsc::unbounded()
		}

		/// The pump must not spuriously time out a *healthy* peer just because the
		/// session is too busy to drain it — the exact failure the split reader had,
		/// where the keep-alive deadline was only polled inside `recv` and went stale
		/// while the session parked handing a stream to a full `accept_*` channel.
		///
		/// Here a capacity-1 channel stands in for that full `accept_*`: the pump
		/// parks delivering the second frame while we sit idle well past the
		/// keep-alive `timeout`, then we drain — and both frames must arrive intact,
		/// with no `Error::Closed` from a phantom timeout.
		#[tokio::test(start_paused = true)]
		async fn survives_a_slow_consumer() {
			let (feed, stream) = mock_stream();
			feed.unbounded_send(Ok(Message::Binary(Bytes::from_static(b"one"))))
				.unwrap();
			feed.unbounded_send(Ok(Message::Binary(Bytes::from_static(b"two"))))
				.unwrap();
			// Keep `feed` alive: the peer stays connected and silent after these two.
			let _feed = feed;

			let ka = KeepAlive::new(Duration::from_millis(10), Duration::from_millis(50));
			let (tx, mut rx) = mpsc::channel(1);
			let pump = tokio::spawn(ws_pump(stream, Some(DeadlineState::new(ka)), usize::MAX, tx));

			// Model a session wedged on a full accept channel: don't read for well
			// over the 50ms keep-alive timeout while the pump is parked on delivery.
			kio::clock::sleep(Duration::from_millis(150)).await;

			assert_eq!(
				rx.recv().await.expect("channel open").expect("no timeout"),
				Bytes::from_static(b"one"),
			);
			assert_eq!(
				rx.recv().await.expect("channel open").expect("no timeout"),
				Bytes::from_static(b"two"),
			);

			pump.abort();
		}

		/// The deadline must still fire for a genuinely silent peer: once the pump is
		/// caught up and reading (not parked on delivery), `timeout` of silence is a
		/// dead peer, surfaced as `Error::Closed`.
		#[tokio::test(start_paused = true)]
		async fn times_out_a_silent_peer() {
			// Held open but never fed — `stream.next()` pends forever.
			let (_feed, stream) = mock_stream();

			let ka = KeepAlive::new(Duration::from_millis(10), Duration::from_millis(50));
			let (tx, mut rx) = mpsc::channel(4);
			let pump = tokio::spawn(ws_pump(stream, Some(DeadlineState::new(ka)), usize::MAX, tx));

			let result = kio::clock::timeout(Duration::from_secs(1), rx.recv())
				.await
				.expect("keep-alive deadline should fire on a silent peer");
			assert!(
				matches!(result, Some(Err(Error::Closed))),
				"expected a keep-alive timeout, got {result:?}"
			);

			pump.abort();
		}

		/// With no keep-alive configured the pump never times out: a silent-but-open
		/// peer just leaves `recv` pending rather than closing.
		#[tokio::test(start_paused = true)]
		async fn no_keep_alive_never_times_out() {
			let (feed, stream) = mock_stream();
			let _feed = feed;

			let (tx, mut rx) = mpsc::channel(4);
			let pump = tokio::spawn(ws_pump(stream, None, usize::MAX, tx));

			// No deadline, so recv stays pending well past any keep-alive window.
			let pending = kio::clock::timeout(Duration::from_millis(100), rx.recv()).await;
			assert!(pending.is_err(), "recv must not resolve without activity");

			pump.abort();
		}

		#[tokio::test]
		async fn skips_empty_messages() {
			let (feed, stream) = mock_stream();
			feed.unbounded_send(Ok(Message::Binary(Bytes::new()))).unwrap();
			feed.unbounded_send(Ok(Message::Binary(Bytes::from_static(b"one"))))
				.unwrap();
			let _feed = feed;

			let (tx, mut rx) = mpsc::channel(4);
			let pump = tokio::spawn(ws_pump(stream, None, usize::MAX, tx));
			assert_eq!(rx.recv().await.unwrap().unwrap(), Bytes::from_static(b"one"));
			pump.abort();
		}

		#[tokio::test]
		async fn rejects_record_exceeding_max_before_queueing() {
			let (feed, stream) = mock_stream();
			feed.unbounded_send(Ok(Message::Binary(Bytes::from_static(b"oversized"))))
				.unwrap();

			let (tx, mut rx) = mpsc::channel(1);
			let pump = tokio::spawn(ws_pump(stream, None, 4, tx));

			assert!(matches!(rx.recv().await, Some(Err(Error::FrameTooLarge))));
			assert!(rx.recv().await.is_none(), "pump must stop after the violation");
			pump.await.unwrap();
		}

		#[tokio::test]
		async fn rejects_a_text_message() {
			let (feed, stream) = mock_stream();
			feed.unbounded_send(Ok(Message::Text("hello".into()))).unwrap();

			let (tx, mut rx) = mpsc::channel(1);
			let pump = tokio::spawn(ws_pump(stream, None, usize::MAX, tx));

			assert!(matches!(rx.recv().await, Some(Err(Error::ProtocolViolation))));
			assert!(rx.recv().await.is_none(), "pump must stop after the violation");
			pump.await.unwrap();
		}
	}
}

#[cfg(feature = "ws")]
pub(crate) use ws_transport::WsTransport;
