//! Incoming streams: their handles, leases, and errors.
//!
//! A read comes straight out of moq-quic's reassembly buffer, which wakes the driver's transmit
//! wait itself when a read owes the peer credit, and the handles wait through moq-quic's poll
//! forms, which wake them.

use std::task::{Poll, ready};

use bytes::{Buf, BufMut, Bytes};
use moq_quic::recv::Chunks;

use super::Shared;
use crate::quic::{Error as ConnectionError, VarInt, stream};

/// A stream that can only be used to receive data.
///
/// Dropped before its end, it is [`stop()`](Self::stop)ped with code 0.
pub struct Stream {
	shared: Shared,
	id: stream::Id,
	end: End,
	/// The code a drop before the end stops with
	drop_stop: VarInt,
}

/// How far a [`Stream`] got, which moq-quic forgets once it frees the stream
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum End {
	Reading,
	/// Every byte up to the finish was read
	Finished,
	/// The peer reset the stream with this code, after the bytes it still delivered
	Reset(VarInt),
}

impl Stream {
	pub(crate) fn new(shared: Shared, id: stream::Id) -> Self {
		shared.hold();
		Self {
			shared,
			id,
			end: End::Reading,
			drop_stop: VarInt::from_u32(0),
		}
	}

	/// Read data contiguously from the stream into `buf`, advancing it.
	///
	/// Waits for some data, then reads as much as has arrived and `buf` can take. Yields the
	/// number of bytes read, or `None` if the stream was finished.
	///
	/// Cancel-safe.
	pub async fn read(&mut self, buf: &mut impl BufMut) -> Result<Option<usize>, Error> {
		kio::wait(|waiter| self.poll_read_buf(waiter, buf)).await
	}

	/// [`Self::read`], as a poll.
	pub(crate) fn poll_read_buf(
		&mut self,
		waiter: &kio::Waiter,
		buf: &mut impl BufMut,
	) -> Poll<Result<Option<usize>, Error>> {
		if !buf.has_remaining_mut() {
			return Poll::Ready(Ok(Some(0)));
		}
		self.poll_read_generic(waiter, |chunks, waiter| fill(chunks, waiter, buf))
	}

	/// Fill `buf` with data from the stream.
	///
	/// Fails with [`Error::FinishedEarly`] if the stream ends first. Not cancel-safe.
	pub async fn read_exact(&mut self, buf: &mut [u8]) -> Result<(), Error> {
		let len = buf.len();
		let mut rest = &mut buf[..];
		while !rest.is_empty() {
			if self.read(&mut rest).await?.is_none() {
				return Err(Error::FinishedEarly(len - rest.len()));
			}
		}
		Ok(())
	}

	/// Read the next segment of data, at most `max` bytes.
	///
	/// Yields `None` if the stream was finished. Unlike [`Self::read()`], this does not copy.
	/// Segment boundaries do not correspond to peer writes, and hence cannot be used as framing.
	///
	/// Cancel-safe.
	#[kio::wrapper]
	pub fn poll_read_chunk(&mut self, waiter: &kio::Waiter, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		self.poll_read_generic(waiter, |chunks, waiter| {
			chunks
				.poll_next(waiter, max)
				.map_ok(|chunk| chunk.map(|chunk| chunk.bytes))
		})
	}

	/// Read all remaining data.
	///
	/// Fails with [`Error::TooLong`] on reading more than `limit` bytes, discarding all data
	/// read. `limit` bounds the returned buffer.
	///
	/// Not cancel-safe.
	pub async fn read_to_end(&mut self, limit: usize) -> Result<Vec<u8>, Error> {
		let mut read = Vec::new();
		while let Some(chunk) = self.read_chunk(usize::MAX).await? {
			if chunk.len() > limit - read.len() {
				return Err(Error::TooLong);
			}
			read.extend_from_slice(&chunk);
		}
		Ok(read)
	}

	/// Lease the stream's unreleased bytes once at least `want` have arrived in order, or the
	/// stream ended. See [`Self::poll_lease()`].
	pub async fn lease(&mut self, want: usize) -> Result<Lease<'_>, Error> {
		let inner = kio::wait(|waiter| self.poll_take(waiter, want)).await?;
		Ok(Lease {
			stream: self,
			inner: Some(inner),
		})
	}

	/// Lease the stream's unreleased bytes once at least `want` have arrived in order, or the
	/// stream ended.
	///
	/// The lease owns the bytes and borrows no connection state, but they keep their connection
	/// flow control credit until read through the lease as a [`Buf`] and then
	/// [`Lease::release`]d; bytes not read are leased again next time. Stream credit still
	/// advances, so a stream window smaller than a message keeps the message flowing.
	///
	/// When the peer has spent the connection window and no reader can make progress, the
	/// stream ranked lowest by [`Self::set_priority`] is evicted: its buffered bytes and their
	/// credit are discarded and its next lease fails with [`Error::Evicted`]. Bytes it took
	/// keep their credit until the stream is stopped. Nothing is evicted while the leased and
	/// unread bytes of every stream stay below the connection receive window, nor while a lease
	/// is out.
	///
	/// Do not mix with the plain reads on one stream, which skip the leased bytes.
	pub fn poll_lease(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		let inner = ready!(self.poll_take(waiter, want))?;
		Poll::Ready(Ok(Lease {
			stream: self,
			inner: Some(inner),
		}))
	}

	/// Take the connection's lease, which [`Lease`] wraps once this returns: the borrow of `self`
	/// it holds cannot start inside a poll that may return `Pending`.
	fn poll_take(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<moq_quic::recv::Lease, Error>> {
		// moq-quic freed the stream once it gave the reset's code to a read.
		if let End::Reset(code) = self.end {
			return Poll::Ready(Err(Error::Reset(code)));
		}
		let result = self
			.shared
			.conn
			.borrow_mut()
			.recv_stream(self.id)
			.poll_lease(waiter, want);
		let err = match ready!(result) {
			Ok(lease) => return Poll::Ready(Ok(lease)),
			Err(moq_quic::recv::Error::Evicted) => Error::Evicted,
			Err(moq_quic::recv::Error::TooLarge) => Error::TooLarge,
			// The stream stays: a smaller lease may still take its reliable bytes.
			Err(err) => {
				let reset = self.shared.conn.borrow_mut().recv_stream(self.id).reset_code();
				match reset {
					Ok(Some(code)) => Error::Reset(code),
					_ => self.lost(err),
				}
			}
		};
		Poll::Ready(Err(err))
	}

	/// Rank this stream for eviction: when the connection window is stuck, the stream with the
	/// lowest priority goes first, and the newest of those that tie.
	pub fn set_priority(&mut self, priority: i64) {
		self.shared
			.conn
			.borrow_mut()
			.recv_stream(self.id)
			.set_priority(priority);
	}

	/// Stop accepting data.
	///
	/// Discards unread data and notifies the peer to stop transmitting.
	pub fn stop(mut self, error_code: VarInt) {
		self.drop_stop = error_code;
	}

	/// The identity of this stream.
	pub fn id(&self) -> stream::Id {
		self.id
	}

	/// Stop with `code`, instead of code 0, if dropped before the end.
	pub(crate) fn set_drop_stop(&mut self, code: VarInt) {
		self.drop_stop = code;
	}

	/// Resolves once the stream has been reset by the peer or otherwise closed.
	///
	/// Yields `Some` with the reset error code when the stream is reset by the peer, and `None`
	/// once every byte the peer finished it with has been read, after which it is no longer
	/// meaningful for the stream to be reset. Fails with [`Error::Evicted`] once the stream was
	/// evicted to free a stuck receive window, until it is stopped. It takes no bytes, so a lease
	/// in progress is left as it was.
	///
	/// Cancel-safe.
	#[kio::wrapper]
	pub fn poll_received_reset(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<VarInt>, Error>> {
		if let End::Reset(code) = self.end {
			return Poll::Ready(Ok(Some(code)));
		}
		let reset = self
			.shared
			.conn
			.borrow_mut()
			.recv_stream(self.id)
			.poll_received_reset(waiter);
		match ready!(reset) {
			// Evicted, so its bytes are gone: nothing frees the window until it is stopped.
			Err(moq_quic::recv::Error::Evicted) => Poll::Ready(Err(Error::Evicted)),
			Ok(code) => {
				// The stream's state was just freed, so later reads fail with the cached code.
				self.end = End::Reset(code);
				Poll::Ready(Ok(Some(code)))
			}
			Err(moq_quic::recv::Error::ConnectionLost) => Poll::Ready(Err(Error::Connection(self.shared.lost()))),
			Err(_) => Poll::Ready(Ok(None)),
		}
	}

	/// Completes once the stream ends: `None` once its finish is read, or `Some` with a reset's
	/// code.
	///
	/// Unlike [`Self::received_reset`], it observes a finish whose bytes were all taken but
	/// whose end was not read yet. An empty read reaches the end without taking the bytes
	/// before it, so it keeps waiting while any are unread.
	pub(crate) fn poll_closed(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<VarInt>, Error>> {
		let read = self.poll_read_generic(waiter, |chunks, waiter| {
			chunks.poll_next(waiter, 0).map_ok(|chunk| chunk.map(|_| ()))
		});
		match ready!(read) {
			// Bytes are still unread, so the end is not reached: wait for a reset, or for
			// more to arrive, as a reader would.
			Ok(Some(())) => self.poll_received_reset(waiter),
			Ok(None) => Poll::Ready(Ok(None)),
			Err(err) => match self.end {
				End::Reset(code) => Poll::Ready(Ok(Some(code))),
				_ => Poll::Ready(Err(err)),
			},
		}
	}

	/// Handle the logic common to every read out of a receive stream.
	///
	/// `read_fn` reads from the passed `Chunks`, waiting on the waiter only when it read
	/// nothing. It yields what it read, or `None` once the stream finished with nothing read.
	fn poll_read_generic<U>(
		&mut self,
		waiter: &kio::Waiter,
		read_fn: impl FnOnce(&mut Chunks<'_>, &kio::Waiter) -> Poll<Result<Option<U>, moq_quic::recv::Error>>,
	) -> Poll<Result<Option<U>, Error>> {
		match self.end {
			End::Reading => {}
			End::Finished => return Poll::Ready(Ok(None)),
			End::Reset(code) => return Poll::Ready(Err(Error::Reset(code))),
		}

		let read = {
			let mut conn = self.shared.conn.borrow_mut();
			match conn.recv_stream(self.id).read() {
				Ok(mut chunks) => {
					let read = read_fn(&mut chunks, waiter);
					// What was read may have reached the finish, after which moq-quic forgets
					// the stream.
					if chunks.is_finished() {
						self.end = End::Finished;
					}
					// Reading is what returns the peer's flow control credit.
					chunks.finalize();
					read
				}
				Err(err) => Poll::Ready(Err(err)),
			}
		};
		match ready!(read) {
			Ok(read) => Poll::Ready(Ok(read)),
			Err(err) => Poll::Ready(Err(self.read_error(err))),
		}
	}

	/// The error for a read or lease moq-quic refused: the peer's reset, or the connection's
	/// loss, which is all that ends a stream the handle still holds.
	fn read_error(&mut self, err: moq_quic::recv::Error) -> Error {
		// Asking for the reset's code frees the stream.
		let reset = self.shared.conn.borrow_mut().recv_stream(self.id).received_reset();
		if let Ok(Some(code)) = reset {
			self.end = End::Reset(code);
			return Error::Reset(code);
		}
		self.lost(err)
	}

	/// The connection's loss, which is the only other way a held receive stream fails.
	fn lost(&self, err: moq_quic::recv::Error) -> Error {
		match self.shared.closed() {
			Some(closed) => Error::Connection(closed),
			None => panic!("a held receive stream failed to read: {err:?}"),
		}
	}
}

/// Copy from `chunks` until `buf` is full or nothing more is readable, returning how much.
///
/// It waits on `waiter` only when it read nothing. What it read is returned first: a reset or
/// the connection's loss after it is reported by the next read.
fn fill(
	chunks: &mut Chunks<'_>,
	waiter: &kio::Waiter,
	buf: &mut impl BufMut,
) -> Poll<Result<Option<usize>, moq_quic::recv::Error>> {
	let noop = kio::Waiter::noop();
	let mut read = 0;
	while buf.has_remaining_mut() {
		let waiter = if read == 0 { waiter } else { &noop };
		match chunks.poll_next(waiter, buf.remaining_mut()) {
			Poll::Ready(Ok(Some(chunk))) => {
				buf.put_slice(&chunk.bytes);
				read += chunk.bytes.len();
			}
			_ if read > 0 => break,
			Poll::Ready(Ok(None)) => return Poll::Ready(Ok(None)),
			Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
			Poll::Pending => return Poll::Pending,
		}
	}
	// `buf` had room, so something was read.
	Poll::Ready(Ok(Some(read)))
}

impl Drop for Stream {
	fn drop(&mut self) {
		if self.end == End::Reading && self.shared.closed().is_none() {
			// Fails harmlessly once the stream is gone.
			let _ = self.shared.conn.borrow_mut().recv_stream(self.id).stop(self.drop_stop);
		}
		self.shared.release();
	}
}

impl std::fmt::Debug for Stream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Stream").field("id", &self.id).finish()
	}
}

/// Unreleased bytes of a [`Stream`] from [`Stream::poll_lease`], read as a [`Buf`].
///
/// It owns the bytes, so the connection carries on while it is held. Hand it back with
/// [`Self::release`] to return the credit of the bytes read; dropping it keeps their credit,
/// as for a message read only in part. Either way the bytes not read are leased again next
/// time.
pub struct Lease<'a> {
	stream: &'a mut Stream,
	/// Taken only to hand it back.
	inner: Option<moq_quic::recv::Lease>,
}

impl Lease<'_> {
	fn inner(&self) -> &moq_quic::recv::Lease {
		self.inner.as_ref().expect("only taken to hand it back")
	}

	fn inner_mut(&mut self) -> &mut moq_quic::recv::Lease {
		self.inner.as_mut().expect("only taken to hand it back")
	}

	/// Whether the peer finished the stream right after the leased bytes.
	pub fn is_fin(&self) -> bool {
		self.inner().is_fin()
	}

	/// The bytes not yet read, in order, one contiguous chunk at a time.
	pub fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		self.inner().chunks()
	}

	/// Return the credit of every byte read so far, by this lease or an earlier one.
	pub fn release(mut self) {
		let lease = self.inner.take().expect("only taken to hand it back");
		let stream = &*self.stream;
		stream.shared.conn.borrow_mut().recv_stream(stream.id).release(lease);
	}
}

/// Reading moves past bytes for good, but they keep their credit until [`Lease::release`].
impl Buf for Lease<'_> {
	fn remaining(&self) -> usize {
		self.inner().remaining()
	}

	fn chunk(&self) -> &[u8] {
		self.inner().chunk()
	}

	fn advance(&mut self, n: usize) {
		self.inner_mut().advance(n);
	}

	fn copy_to_bytes(&mut self, n: usize) -> Bytes {
		self.inner_mut().copy_to_bytes(n)
	}
}

impl Drop for Lease<'_> {
	fn drop(&mut self) {
		if let Some(lease) = self.inner.take() {
			let stream = &*self.stream;
			stream.shared.conn.borrow_mut().recv_stream(stream.id).restore(lease);
		}
	}
}

impl std::fmt::Debug for Lease<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Lease")
			.field("stream", &self.stream.id)
			.finish_non_exhaustive()
	}
}

/// Errors from reading or leasing a stream.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
	/// The peer abandoned transmitting data on this stream, with its application error code.
	#[error("stream reset by peer: error {0}")]
	Reset(VarInt),
	/// The connection window was stuck and this stream was discarded to free it.
	///
	/// See [`Stream::poll_lease`]. The peer hears of it once the stream is stopped, so call
	/// [`Stream::stop`] with your own code: dropping the stream sends code 0.
	#[error("evicted")]
	Evicted,
	/// The stream finished before [`Stream::read_exact`] filled its buffer, after this many bytes.
	#[error("stream finished early ({0} bytes read)")]
	FinishedEarly(usize),
	/// The stream is longer than [`Stream::read_to_end`]'s limit.
	#[error("stream too long")]
	TooLong,
	/// A lease wanted more than the connection receive window can hold.
	#[error("want exceeds the receive window")]
	TooLarge,
	/// The connection was lost.
	#[error("connection lost: {0}")]
	Connection(#[from] ConnectionError),
}

impl From<Error> for std::io::Error {
	fn from(x: Error) -> Self {
		let kind = match x {
			Error::Reset(_) | Error::Evicted => std::io::ErrorKind::ConnectionReset,
			Error::FinishedEarly(_) => std::io::ErrorKind::UnexpectedEof,
			Error::TooLong | Error::TooLarge => std::io::ErrorKind::InvalidInput,
			Error::Connection(_) => std::io::ErrorKind::NotConnected,
		};
		Self::new(kind, x)
	}
}

/// Preserve a stream failure when a connection-level operation, such as framing, reports it.
impl From<Error> for ConnectionError {
	fn from(err: Error) -> Self {
		match err {
			Error::Reset(code) => Self::Reset(code.into_inner()),
			Error::Evicted => Self::Evicted,
			Error::TooLarge => Self::TooLarge,
			Error::Connection(err) => err,
			err => Self::Quic(err.to_string()),
		}
	}
}
