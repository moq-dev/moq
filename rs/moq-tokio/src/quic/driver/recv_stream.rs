use std::{
	future::{Future, poll_fn},
	io,
	pin::Pin,
	task::{Context, Poll, ready},
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use moq_quic::{
	Chunk, Chunks, ClosedStream, LeaseError, ReadableError, StreamId, connection::Error as ConnectionError,
};
use thiserror::Error;
use tokio::io::ReadBuf;

use crate::quic::driver::{
	VarInt,
	connection::{ConnectionRef, State},
	mutex::MutexGuard,
};

/// A stream that can only be used to receive data
///
/// `stop(0)` is implicitly called on drop unless:
/// - A variant of [`ReadError`] has been yielded by a read call
/// - [`stop()`] was called explicitly
///
/// # Cancellation
///
/// A `read` method is said to be *cancel-safe* when dropping its future before the future becomes
/// ready cannot lead to loss of stream data. This is true of methods which succeed immediately when
/// any progress is made, and is not true of methods which might need to perform multiple reads
/// internally before succeeding. Each `read` method documents whether it is cancel-safe.
///
/// # Common issues
///
/// ## Data never received on a locally-opened stream
///
/// Peers are not notified of streams until they or a later-numbered stream are used to send
/// data. If a bidirectional stream is locally opened but never used to send, then the peer may
/// never see it. Application protocols should always arrange for the endpoint which will first
/// transmit on a stream to be the endpoint responsible for opening it.
///
/// ## Data never received on a remotely-opened stream
///
/// Verify that the stream you are receiving is the same one that the server is sending on, e.g. by
/// logging the [`id`] of each. Streams are always accepted in the same order as they are created,
/// i.e. ascending order by [`StreamId`]. For example, even if a sender first transmits on
/// bidirectional stream 1, the first stream yielded by [`Connection::accept_bi`] on the receiver
/// will be bidirectional stream 0.
///
/// [`ReadError`]: crate::quic::driver::ReadError
/// [`stop()`]: RecvStream::stop
/// [`SendStream::finish`]: crate::quic::driver::SendStream::finish
/// [`WriteError::Stopped`]: crate::quic::driver::WriteError::Stopped
/// [`id`]: RecvStream::id
/// [`Connection::accept_bi`]: crate::quic::driver::connection::Connection::accept_bi
#[derive(Debug)]
pub struct RecvStream {
	conn: ConnectionRef,
	stream: StreamId,
	all_data_read: bool,
	reset: Option<VarInt>,
	/// The code a drop before the end stops with
	drop_stop: VarInt,
}

impl RecvStream {
	pub(crate) fn new(conn: ConnectionRef, stream: StreamId) -> Self {
		Self {
			conn,
			stream,
			all_data_read: false,
			reset: None,
			drop_stop: VarInt::from_u32(0),
		}
	}

	/// Read data contiguously from the stream.
	///
	/// Yields the number of bytes read into `buf` on success, or `None` if the stream was finished.
	///
	/// This operation is cancel-safe.
	pub async fn read(&mut self, buf: &mut [u8]) -> Result<Option<usize>, ReadError> {
		Read {
			stream: self,
			buf: ReadBuf::new(buf),
		}
		.await
	}

	/// Read an exact number of bytes contiguously from the stream.
	///
	/// See [`read()`] for details. This operation is *not* cancel-safe.
	///
	/// [`read()`]: RecvStream::read
	pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), ReadExactError> {
		ReadExact {
			stream: self,
			buf: ReadBuf::new(buf),
		}
		.await
	}

	/// Lease the stream's unreleased bytes once at least `want` have arrived in order, or the
	/// stream ended.
	///
	/// The bytes stay in the connection's receive buffer, keeping their connection flow control
	/// credit, until read through the lease as a [`Buf`] and then [`Lease::release`]d; bytes not
	/// read stay for the next lease. Stream credit still advances, so a stream window smaller
	/// than a message keeps the message flowing.
	///
	/// When the peer has spent the connection window and no reader can make progress, the
	/// stream ranked lowest by [`Self::set_priority`] is evicted: its buffered bytes and their
	/// credit are discarded and its next lease fails with [`ReadError::Evicted`]. Bytes it took
	/// keep their credit until the stream is stopped. Nothing is evicted while the leased and
	/// unread bytes of every stream stay below the connection receive window.
	///
	/// The lease holds the connection's lock: drop it before touching any other stream.
	/// Do not mix with the plain reads on one stream, which skip the leased bytes.
	pub fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<Lease<'_>, ReadError>> {
		let mut conn = self.conn.state.lock();
		let err = match conn.inner.recv_stream(self.stream).lease(want) {
			Ok(transmit) => {
				if transmit.should_transmit() {
					conn.wake();
				}
				return Poll::Ready(Ok(Lease {
					conn,
					stream: self.stream,
				}));
			}
			Err(LeaseError::Blocked(transmit)) => {
				if transmit.should_transmit() {
					conn.wake();
				}
				if let Some(x) = &conn.error {
					return Poll::Ready(Err(ReadError::ConnectionLost(x.clone())));
				}
				conn.blocked_readers.insert(self.stream, cx.waker().clone());
				return Poll::Pending;
			}
			Err(LeaseError::Reset(code)) => ReadError::Reset(code),
			Err(LeaseError::Evicted) => ReadError::Evicted,
			Err(LeaseError::TooLarge) => ReadError::TooLarge,
			Err(LeaseError::ClosedStream) => match &conn.error {
				Some(x) => ReadError::ConnectionLost(x.clone()),
				None => ReadError::ClosedStream,
			},
		};
		conn.blocked_readers.remove(&self.stream);
		Poll::Ready(Err(err))
	}

	/// Rank this stream for eviction: when the connection window is stuck, the stream with the
	/// lowest priority goes first, and the newest of those that tie.
	pub fn set_priority(&mut self, priority: i64) {
		let mut conn = self.conn.state.lock();
		conn.inner.recv_stream(self.stream).set_priority(priority);
	}

	/// Attempts to read from the stream into the provided buffer
	///
	/// On success, returns `Poll::Ready(Ok(num_bytes_read))` and places data into `buf`. If this
	/// returns zero bytes read (and `buf` has a non-zero length), that indicates that the remote
	/// side has [`finish`]ed the stream and the local side has already read all bytes.
	///
	/// If no data is available for reading, this returns `Poll::Pending` and arranges for the
	/// current task (via `cx.waker()`) to be notified when the stream becomes readable or is
	/// closed.
	///
	/// [`finish`]: crate::quic::driver::SendStream::finish
	pub fn poll_read(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize, ReadError>> {
		let mut buf = ReadBuf::new(buf);
		ready!(self.poll_read_buf(cx, &mut buf))?;
		Poll::Ready(Ok(buf.filled().len()))
	}

	/// Attempts to read from the stream into the provided buffer, which may be uninitialized
	///
	/// On success, returns `Poll::Ready(Ok(()))` and places data into the unfilled portion of
	/// `buf`. If this does not write any bytes to `buf` (and `buf.remaining()` is non-zero), that
	/// indicates that the remote side has [`finish`]ed the stream and the local side has already
	/// read all bytes.
	///
	/// If no data is available for reading, this returns `Poll::Pending` and arranges for the
	/// current task (via `cx.waker()`) to be notified when the stream becomes readable or is
	/// closed.
	///
	/// [`finish`]: crate::quic::driver::SendStream::finish
	fn poll_read_buf(&mut self, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<Result<(), ReadError>> {
		if buf.remaining() == 0 {
			return Poll::Ready(Ok(()));
		}

		self.poll_read_generic(cx, |chunks| fill(chunks, buf))
			.map(|res| res.map(|_| ()))
	}

	/// Read the next segment of data
	///
	/// Yields `None` if the stream was finished, or the next contiguous segment of data.
	/// Slightly more efficient than `read` due to not copying. Chunk boundaries do not correspond
	/// to peer writes, and hence cannot be used as framing.
	///
	/// This operation is cancel-safe.
	pub async fn read_chunk(&mut self, max_length: usize) -> Result<Option<Chunk>, ReadError> {
		ReadChunk {
			stream: self,
			max_length,
		}
		.await
	}

	/// Attempts to read a chunk from the stream.
	///
	/// On success, returns `Poll::Ready(Ok(Some(chunk)))`. If `Poll::Ready(Ok(None))`
	/// is returned, it implies that EOF has been reached.
	///
	/// If no data is available for reading, the method returns `Poll::Pending`
	/// and arranges for the current task (via cx.waker()) to receive a notification
	/// when the stream becomes readable or is closed.
	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max_length: usize) -> Poll<Result<Option<Chunk>, ReadError>> {
		self.poll_read_generic(cx, |chunks| match chunks.next(max_length) {
			Ok(Some(chunk)) => ReadStatus::Readable(chunk),
			res => (None, res.err()).into(),
		})
	}

	/// Convenience method to read all remaining data into a buffer
	///
	/// Fails with [`ReadToEndError::TooLong`] on reading more than `size_limit` bytes, discarding
	/// all data read. `size_limit` bounds the returned buffer.
	///
	/// This operation is *not* cancel-safe.
	///
	/// [`ReadToEndError::TooLong`]: crate::quic::driver::ReadToEndError::TooLong
	pub async fn read_to_end(&mut self, size_limit: usize) -> Result<Vec<u8>, ReadToEndError> {
		let mut buffer = ReadToEndBuffer::new(size_limit);
		while let Some(chunk) = self.read_chunk(usize::MAX).await? {
			buffer.push(chunk)?;
		}
		Ok(buffer.finish())
	}

	/// Stop accepting data
	///
	/// Discards unread data and notifies the peer to stop transmitting. Once stopped, further
	/// attempts to operate on a stream will yield `ClosedStream` errors.
	pub fn stop(&mut self, error_code: VarInt) -> Result<(), ClosedStream> {
		let mut conn = self.conn.state.lock();
		conn.inner.recv_stream(self.stream).stop(error_code)?;
		conn.wake();
		self.all_data_read = true;
		// Clean up shared state that might be left over from a cancalled read
		// operation, so `drop` doesn't have to
		conn.blocked_readers.remove(&self.stream);
		Ok(())
	}

	/// Get the identity of this stream
	pub fn id(&self) -> StreamId {
		self.stream
	}

	/// Stop with `code`, instead of code 0, if dropped before the end
	#[cfg(feature = "quic")]
	pub(crate) fn set_drop_stop(&mut self, code: VarInt) {
		self.drop_stop = code;
	}

	/// Completes when the stream has been reset by the peer or otherwise closed
	///
	/// Yields `Some` with the reset error code when the stream is reset by the peer. Yields `None`
	/// when the stream was previously [`stop()`](Self::stop)ed, or when the stream was
	/// [`finish()`](crate::quic::driver::SendStream::finish)ed by the peer and all data has been received, after
	/// which it is no longer meaningful for the stream to be reset.
	///
	/// This operation is cancel-safe.
	pub async fn received_reset(&mut self) -> Result<Option<VarInt>, ResetError> {
		poll_fn(|cx| {
			let mut conn = self.conn.state.lock();

			if let Some(code) = self.reset {
				return Poll::Ready(Ok(Some(code)));
			}

			match conn.inner.recv_stream(self.stream).received_reset() {
				Err(_) => Poll::Ready(Ok(None)),
				Ok(Some(error_code)) => {
					// Stream state has just now been freed, so the connection may need to issue new
					// stream ID flow control credit
					conn.wake();
					Poll::Ready(Ok(Some(error_code)))
				}
				Ok(None) => {
					if let Some(e) = &conn.error {
						return Poll::Ready(Err(e.clone().into()));
					}
					// Resets always notify readers, since a reset is an immediate read error. We
					// could introduce a dedicated channel to reduce the risk of spurious wakeups,
					// but that increased complexity is probably not justified, as an application
					// that is expecting a reset is not likely to receive large amounts of data.
					conn.blocked_readers.insert(self.stream, cx.waker().clone());
					Poll::Pending
				}
			}
		})
		.await
	}

	/// Handle common logic related to reading out of a receive stream
	///
	/// This takes an `FnMut` closure that takes care of the actual reading process, matching
	/// the detailed read semantics for the calling function with a particular return type.
	/// The closure can read from the passed `&mut Chunks` and has to return the status after
	/// reading: the amount of data read, and the status after the final read call.
	fn poll_read_generic<T, U>(&mut self, cx: &mut Context<'_>, mut read_fn: T) -> Poll<Result<Option<U>, ReadError>>
	where
		T: FnMut(&mut Chunks<'_>) -> ReadStatus<U>,
	{
		use moq_quic::ReadError::*;
		use std::collections::hash_map::Entry;

		if self.all_data_read {
			return Poll::Ready(Ok(None));
		}

		let mut conn = self.conn.state.lock();

		// If we stored an error during a previous call, return it now. This can happen if a
		// `read_fn` both wants to return data and also returns an error in its final stream status.
		let status = match self.reset {
			Some(code) => ReadStatus::Failed(None, Reset(code)),
			None => {
				let mut recv = conn.inner.recv_stream(self.stream);
				let mut chunks = recv.read()?;
				let status = read_fn(&mut chunks);
				if chunks.finalize().should_transmit() {
					conn.wake();
				}
				status
			}
		};

		match status {
			ReadStatus::Readable(read) => Poll::Ready(Ok(Some(read))),
			ReadStatus::Finished(read) => {
				self.all_data_read = true;
				// Stream is dead, cleanup stale waker
				conn.blocked_readers.remove(&self.stream);
				Poll::Ready(Ok(read))
			}
			ReadStatus::Failed(read, Blocked) => match read {
				Some(val) => Poll::Ready(Ok(Some(val))),
				None => {
					if let Some(x) = &conn.error {
						return Poll::Ready(Err(ReadError::ConnectionLost(x.clone())));
					}
					match conn.blocked_readers.entry(self.stream) {
						Entry::Occupied(mut entry) => {
							entry.get_mut().clone_from(cx.waker());
						}
						Entry::Vacant(entry) => {
							entry.insert(cx.waker().clone());
						}
					}
					Poll::Pending
				}
			},
			ReadStatus::Failed(read, Reset(error_code)) => match read {
				None => {
					self.all_data_read = true;
					self.reset = Some(error_code);
					// Stream is dead, cleanup stale waker
					conn.blocked_readers.remove(&self.stream);
					Poll::Ready(Err(ReadError::Reset(error_code)))
				}
				done => {
					self.reset = Some(error_code);
					Poll::Ready(Ok(done))
				}
			},
		}
	}
}

/// Copy from `chunks` until `buf` is full or nothing more is readable.
fn fill(chunks: &mut Chunks<'_>, buf: &mut ReadBuf<'_>) -> ReadStatus<()> {
	let mut read = false;
	loop {
		if buf.remaining() == 0 {
			// We know `read` is `true` because `buf.remaining()` was not 0 before
			return ReadStatus::Readable(());
		}

		match chunks.next(buf.remaining()) {
			Ok(Some(chunk)) => {
				buf.put_slice(&chunk.bytes);
				read = true;
			}
			res => return (if read { Some(()) } else { None }, res.err()).into(),
		}
	}
}

/// Unreleased bytes borrowed from a [`RecvStream`] by [`RecvStream::poll_lease`], read as a
/// [`Buf`].
///
/// Holds the connection's lock for as long as it lives.
pub struct Lease<'a> {
	conn: MutexGuard<'a, State>,
	stream: StreamId,
}

impl Lease<'_> {
	fn leased(&self) -> &moq_quic::Leased {
		self.conn
			.inner
			.leased(self.stream)
			.expect("a lease outlives nothing it borrows")
	}

	/// Whether the peer finished the stream right after the leased bytes.
	pub fn is_fin(&self) -> bool {
		self.leased().is_fin()
	}

	/// The bytes not yet read, in order, one contiguous chunk at a time.
	pub fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		self.leased().chunks()
	}

	/// Return the credit of every byte read so far, by this lease or an earlier one.
	pub fn release(mut self) {
		if self.conn.inner.recv_stream(self.stream).release().should_transmit() {
			self.conn.wake();
		}
	}
}

/// Reading moves past bytes for good, but they keep their credit until [`Lease::release`].
impl Buf for Lease<'_> {
	fn remaining(&self) -> usize {
		self.leased().len()
	}

	fn chunk(&self) -> &[u8] {
		self.leased().chunks().next().unwrap_or_default()
	}

	fn advance(&mut self, n: usize) {
		self.conn.inner.recv_stream(self.stream).advance(n);
	}

	fn copy_to_bytes(&mut self, n: usize) -> Bytes {
		match n <= self.chunk().len() {
			true => self.conn.inner.recv_stream(self.stream).take(n),
			false => {
				let mut bytes = BytesMut::with_capacity(n);
				bytes.put((&mut *self).take(n));
				bytes.freeze()
			}
		}
	}
}

enum ReadStatus<T> {
	Readable(T),
	Finished(Option<T>),
	Failed(Option<T>, moq_quic::ReadError),
}

impl<T> From<(Option<T>, Option<moq_quic::ReadError>)> for ReadStatus<T> {
	fn from(status: (Option<T>, Option<moq_quic::ReadError>)) -> Self {
		match status {
			(read, None) => Self::Finished(read),
			(read, Some(e)) => Self::Failed(read, e),
		}
	}
}

struct ReadToEndBuffer {
	read: Vec<u8>,
	size_limit: usize,
}

impl ReadToEndBuffer {
	fn new(size_limit: usize) -> Self {
		Self {
			read: Vec::new(),
			size_limit,
		}
	}

	fn push(&mut self, chunk: Chunk) -> Result<(), ReadToEndError> {
		if chunk.bytes.len() > self.size_limit - self.read.len() {
			return Err(ReadToEndError::TooLong);
		}
		self.read.extend_from_slice(&chunk.bytes);
		Ok(())
	}

	fn finish(self) -> Vec<u8> {
		self.read
	}
}

/// Errors from [`RecvStream::read_to_end`]
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadToEndError {
	/// An error occurred during reading
	#[error("read error: {0}")]
	Read(#[from] ReadError),
	/// The stream is larger than the user-supplied limit
	#[error("stream too long")]
	TooLong,
}

impl tokio::io::AsyncRead for RecvStream {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		ready!(Self::poll_read_buf(self.get_mut(), cx, buf))?;
		Poll::Ready(Ok(()))
	}
}

impl Drop for RecvStream {
	fn drop(&mut self) {
		if self.all_data_read {
			debug_assert!(
				!self.conn.state.lock().blocked_readers.contains_key(&self.stream),
				"Stream {} should not have a blocked reader when all data read is true",
				self.stream
			);
			return;
		}
		let mut conn = self.conn.state.lock();

		// clean up any previously registered wakers
		conn.blocked_readers.remove(&self.stream);

		if conn.error.is_some() {
			return;
		}

		// Ignore ClosedStream errors
		let _ = conn.inner.recv_stream(self.stream).stop(self.drop_stop);
		conn.wake();
	}
}

/// Errors that arise from reading from a stream.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadError {
	/// The peer abandoned transmitting data on this stream
	///
	/// Carries an application-defined error code.
	#[error("stream reset by peer: error {0}")]
	Reset(VarInt),
	/// The connection was lost
	#[error("connection lost")]
	ConnectionLost(#[from] ConnectionError),
	/// The stream has already been stopped, finished, or reset
	#[error("closed stream")]
	ClosedStream,
	/// The connection window was stuck and this stream was discarded to free it
	///
	/// See [`RecvStream::poll_lease`]. The peer hears of it once the stream is stopped, so call
	/// [`RecvStream::stop`] with your own code: dropping the stream sends code 0.
	#[error("evicted")]
	Evicted,
	/// A lease wanted more than the connection receive window can hold
	#[error("want exceeds the receive window")]
	TooLarge,
}

impl From<ReadableError> for ReadError {
	fn from(e: ReadableError) -> Self {
		match e {
			ReadableError::ClosedStream => Self::ClosedStream,
		}
	}
}

impl From<ResetError> for ReadError {
	fn from(e: ResetError) -> Self {
		match e {
			ResetError::ConnectionLost(e) => Self::ConnectionLost(e),
		}
	}
}

impl From<ReadError> for io::Error {
	fn from(x: ReadError) -> Self {
		use ReadError::*;
		let kind = match x {
			Reset { .. } | Evicted => io::ErrorKind::ConnectionReset,
			ConnectionLost(_) | ClosedStream => io::ErrorKind::NotConnected,
			TooLarge => io::ErrorKind::InvalidInput,
		};
		Self::new(kind, x)
	}
}

/// Errors that arise while waiting for a stream to be reset
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ResetError {
	/// The connection was lost
	#[error("connection lost")]
	ConnectionLost(#[from] ConnectionError),
}

impl From<ResetError> for io::Error {
	fn from(x: ResetError) -> Self {
		use ResetError::*;
		let kind = match x {
			ConnectionLost(_) => io::ErrorKind::NotConnected,
		};
		Self::new(kind, x)
	}
}

/// Future produced by [`RecvStream::read()`].
///
/// [`RecvStream::read()`]: crate::quic::driver::RecvStream::read
struct Read<'a> {
	stream: &'a mut RecvStream,
	buf: ReadBuf<'a>,
}

impl Future for Read<'_> {
	type Output = Result<Option<usize>, ReadError>;

	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		ready!(this.stream.poll_read_buf(cx, &mut this.buf))?;
		match this.buf.filled().len() {
			0 if this.buf.capacity() != 0 => Poll::Ready(Ok(None)),
			n => Poll::Ready(Ok(Some(n))),
		}
	}
}

/// Future produced by [`RecvStream::read_exact()`].
///
/// [`RecvStream::read_exact()`]: crate::quic::driver::RecvStream::read_exact
struct ReadExact<'a> {
	stream: &'a mut RecvStream,
	buf: ReadBuf<'a>,
}

impl Future for ReadExact<'_> {
	type Output = Result<(), ReadExactError>;
	fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let this = self.get_mut();
		let mut remaining = this.buf.remaining();
		while remaining > 0 {
			ready!(this.stream.poll_read_buf(cx, &mut this.buf))?;
			let new = this.buf.remaining();
			if new == remaining {
				return Poll::Ready(Err(ReadExactError::FinishedEarly(this.buf.filled().len())));
			}
			remaining = new;
		}
		Poll::Ready(Ok(()))
	}
}

/// Errors that arise from reading from a stream.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadExactError {
	/// The stream finished before all bytes were read
	#[error("stream finished early ({0} bytes read)")]
	FinishedEarly(usize),
	/// A read error occurred
	#[error(transparent)]
	ReadError(#[from] ReadError),
}

/// Future produced by [`RecvStream::read_chunk()`].
///
/// [`RecvStream::read_chunk()`]: crate::quic::driver::RecvStream::read_chunk
struct ReadChunk<'a> {
	stream: &'a mut RecvStream,
	max_length: usize,
}

impl Future for ReadChunk<'_> {
	type Output = Result<Option<Chunk>, ReadError>;
	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let max_length = self.max_length;
		self.stream.poll_read_chunk(cx, max_length)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn read_to_end_limit_covers_all_chunks() {
		let mut buffer = ReadToEndBuffer::new(4);
		buffer
			.push(Chunk {
				bytes: Bytes::from_static(b"ab"),
			})
			.unwrap();
		buffer
			.push(Chunk {
				bytes: Bytes::from_static(b"cd"),
			})
			.unwrap();
		assert_eq!(
			buffer.push(Chunk {
				bytes: Bytes::from_static(b"e")
			}),
			Err(ReadToEndError::TooLong)
		);
		assert_eq!(buffer.finish(), b"abcd");
		let mut empty = ReadToEndBuffer::new(0);
		assert_eq!(
			empty.push(Chunk {
				bytes: Bytes::from_static(b"x")
			}),
			Err(ReadToEndError::TooLong)
		);
		assert!(empty.finish().is_empty());
	}
}
