//! Incoming streams: their handles, leases, and errors.

use std::{
	io,
	pin::Pin,
	task::{Context, Poll, ready},
};

use bytes::{Buf, BufMut, Bytes};
use moq_quic::{
	connection::Error as ConnectionError,
	recv::{Chunk, Chunks},
};
use thiserror::Error;
use tokio::io::ReadBuf;

use crate::quic::driver::{VarInt, connection::ConnectionRef, stream};

/// A stream that can only be used to receive data
///
/// Dropped before its end, it is [`stop()`](Self::stop)ped with code 0.
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
/// i.e. ascending order by [`stream::Id`]. For example, even if a sender first transmits on
/// bidirectional stream 1, the first stream yielded by [`Connection::accept_bi`] on the receiver
/// will be bidirectional stream 0.
///
/// [`id`]: Stream::id
/// [`Connection::accept_bi`]: crate::quic::driver::connection::Connection::accept_bi
#[derive(Debug)]
pub struct Stream {
	reader: Reader,
	/// Holds the waiter of a read polled from a [`Context`], by
	/// [`AsyncRead`](tokio::io::AsyncRead) or [`Self::poll_lease`], across polls
	park: kio::Park,
}

/// The state a [`Stream`]'s reads borrow, apart from its park
#[derive(Debug)]
struct Reader {
	conn: ConnectionRef,
	id: stream::Id,
	all_data_read: bool,
	reset: Option<VarInt>,
	/// The code a drop before the end stops with
	drop_stop: VarInt,
}

impl Stream {
	pub(crate) fn new(conn: ConnectionRef, id: stream::Id) -> Self {
		Self {
			reader: Reader {
				conn,
				id,
				all_data_read: false,
				reset: None,
				drop_stop: VarInt::from_u32(0),
			},
			park: kio::Park::default(),
		}
	}

	/// Read data contiguously from the stream into `buf`, advancing it
	///
	/// Waits for some data, then reads as much as has arrived and `buf` can take. Yields the
	/// number of bytes read, or `None` if the stream was finished.
	///
	/// This operation is cancel-safe.
	pub async fn read(&mut self, buf: &mut impl BufMut) -> Result<Option<usize>, Error> {
		kio::wait(|waiter| self.reader.poll_read_buf(waiter, buf)).await
	}

	/// Fill `buf` with data from the stream
	///
	/// Fails with [`Error::FinishedEarly`] if the stream ends first. This operation is *not*
	/// cancel-safe.
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

	/// Read the next segment of data, at most `max` bytes
	///
	/// Yields `None` if the stream was finished. Unlike [`Self::read()`], this does not copy.
	/// Segment boundaries do not correspond to peer writes, and hence cannot be used as framing.
	///
	/// This operation is cancel-safe.
	pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
		kio::wait(|waiter| {
			self.reader
				.poll_read_generic(waiter, |chunks, waiter| match poll_next(chunks, waiter, max) {
					Ok(Some(chunk)) => ReadStatus::Readable(chunk.bytes),
					res => (None, res.err()).into(),
				})
		})
		.await
	}

	/// Read all remaining data
	///
	/// Fails with [`Error::TooLong`] on reading more than `limit` bytes, discarding all data
	/// read. `limit` bounds the returned buffer.
	///
	/// This operation is *not* cancel-safe.
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
		let inner = kio::wait(|waiter| self.reader.poll_take(waiter, want)).await?;
		Ok(Lease {
			reader: &mut self.reader,
			inner: Some(inner),
		})
	}

	/// Lease the stream's unreleased bytes once at least `want` have arrived in order, or the
	/// stream ended.
	///
	/// The lease owns the bytes and holds no lock, but they keep their connection flow control
	/// credit until read through the lease as a [`Buf`] and then [`Lease::release`]d; bytes not
	/// read are leased again next time. Stream credit still advances, so a stream window smaller
	/// than a message keeps the message flowing.
	///
	/// When the peer has spent the connection window and no reader can make progress, the
	/// stream ranked lowest by [`Self::set_priority`] is evicted: its buffered bytes and their
	/// credit are discarded and its next lease fails with [`Error::Evicted`]. Bytes it took
	/// keep their credit until the stream is stopped. Nothing is evicted while the leased and
	/// unread bytes of every stream stay below the connection receive window, nor while a lease
	/// is out.
	///
	/// Do not mix with the plain reads on one stream, which skip the leased bytes.
	pub fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		let waiter = self.park.hold(cx);
		let inner = ready!(self.reader.poll_take(waiter, want))?;
		Poll::Ready(Ok(Lease {
			reader: &mut self.reader,
			inner: Some(inner),
		}))
	}

	/// Rank this stream for eviction: when the connection window is stuck, the stream with the
	/// lowest priority goes first, and the newest of those that tie.
	pub fn set_priority(&mut self, priority: i64) {
		let mut conn = self.reader.conn.state.lock();
		conn.inner.recv_stream(self.reader.id).set_priority(priority);
	}

	/// Stop accepting data
	///
	/// Discards unread data and notifies the peer to stop transmitting.
	pub fn stop(mut self, error_code: VarInt) {
		self.reader.drop_stop = error_code;
	}

	/// The identity of this stream
	pub fn id(&self) -> stream::Id {
		self.reader.id
	}

	/// Stop with `code`, instead of code 0, if dropped before the end
	#[cfg(feature = "quic")]
	pub(crate) fn set_drop_stop(&mut self, code: VarInt) {
		self.reader.drop_stop = code;
	}

	/// Completes when the stream has been reset by the peer or otherwise closed
	///
	/// Yields `Some` with the reset error code when the stream is reset by the peer. Yields `None`
	/// when the stream was [`finish()`](crate::quic::driver::send::Stream::finish)ed by the peer
	/// and all data has been received, after which it is no longer meaningful for the stream to
	/// be reset.
	///
	/// This operation is cancel-safe.
	pub async fn received_reset(&mut self) -> Result<Option<VarInt>, Error> {
		kio::wait(|waiter| self.reader.poll_received_reset(waiter)).await
	}
}

impl Reader {
	/// Take the connection's lease, which [`Lease`] wraps once this returns: the borrow of `self`
	/// it holds cannot start inside a poll that may return `Pending`.
	fn poll_take(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<moq_quic::recv::Lease, Error>> {
		// moq-quic freed the stream when it reported the reset.
		if let Some(code) = self.reset {
			return Poll::Ready(Err(Error::Reset(code)));
		}
		let mut conn = self.conn.state.lock();
		// moq-quic wakes the driver for any credit the lease returned.
		let (result, _) = conn.inner.recv_stream(self.id).poll_lease(waiter, want);
		let err = match ready!(result) {
			Ok(lease) => return Poll::Ready(Ok(lease)),
			Err(err) => err,
		};
		let closed = conn.error();
		drop(conn);
		Poll::Ready(Err(lease_error(err, closed)))
	}

	fn poll_read_buf(&mut self, waiter: &kio::Waiter, buf: &mut impl BufMut) -> Poll<Result<Option<usize>, Error>> {
		if !buf.has_remaining_mut() {
			return Poll::Ready(Ok(Some(0)));
		}
		self.poll_read_generic(waiter, |chunks, waiter| fill(chunks, waiter, buf))
	}

	fn poll_received_reset(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<VarInt>, Error>> {
		if let Some(code) = self.reset {
			return Poll::Ready(Ok(Some(code)));
		}
		let mut conn = self.conn.state.lock();
		Poll::Ready(
			match ready!(conn.inner.recv_stream(self.id).poll_received_reset(waiter)) {
				Ok(error_code) => {
					// Stream state has just now been freed, so later reads fail with the cached code.
					self.reset = Some(error_code);
					Ok(Some(error_code))
				}
				Err(moq_quic::recv::Error::ConnectionLost) => Err(Error::Connection(conn.lost())),
				Err(_) => Ok(None),
			},
		)
	}

	/// Handle common logic related to reading out of a receive stream
	///
	/// This takes an `FnMut` closure that takes care of the actual reading process, matching
	/// the detailed read semantics for the calling function with a particular return type.
	/// The closure can read from the passed `&mut Chunks` and has to return the status after
	/// reading: the amount of data read, and the status after the final read call. It reports
	/// [`Blocked`](moq_quic::recv::Error::Blocked) with nothing read only once it registered
	/// the waiter, as [`poll_next`] does.
	fn poll_read_generic<T, U>(&mut self, waiter: &kio::Waiter, mut read_fn: T) -> Poll<Result<Option<U>, Error>>
	where
		T: FnMut(&mut Chunks<'_>, &kio::Waiter) -> ReadStatus<U>,
	{
		use moq_quic::recv::Error::{Blocked, Reset};

		if self.all_data_read {
			return Poll::Ready(Ok(None));
		}

		let mut conn = self.conn.state.lock();

		// If we stored an error during a previous call, return it now. This can happen if a
		// `read_fn` both wants to return data and also returns an error in its final stream status.
		let status = match self.reset {
			Some(code) => ReadStatus::Failed(None, Reset(code)),
			None => {
				let mut recv = conn.inner.recv_stream(self.id);
				match recv.read() {
					Ok(mut chunks) => {
						let status = read_fn(&mut chunks, waiter);
						// moq-quic wakes the driver for the credit the read returned.
						let _ = chunks.finalize();
						status
					}
					Err(err) => ReadStatus::Failed(None, err),
				}
			}
		};

		match status {
			ReadStatus::Readable(read) => Poll::Ready(Ok(Some(read))),
			ReadStatus::Finished(read) => {
				self.all_data_read = true;
				Poll::Ready(Ok(read))
			}
			ReadStatus::Failed(Some(read), Blocked) => Poll::Ready(Ok(Some(read))),
			// moq-quic registered the waiter.
			ReadStatus::Failed(None, Blocked) => Poll::Pending,
			ReadStatus::Failed(read, Reset(error_code)) => {
				self.reset = Some(error_code);
				if read.is_some() {
					return Poll::Ready(Ok(read));
				}
				self.all_data_read = true;
				Poll::Ready(Err(Error::Reset(error_code)))
			}
			ReadStatus::Failed(_, err) => {
				let error = conn.error();
				drop(conn);
				Poll::Ready(Err(read_error(err, error)))
			}
		}
	}
}

/// [`Chunks::poll_next`], reporting a registered wait as [`Blocked`](moq_quic::recv::Error::Blocked)
fn poll_next(
	chunks: &mut Chunks<'_>,
	waiter: &kio::Waiter,
	max: usize,
) -> Result<Option<Chunk>, moq_quic::recv::Error> {
	match chunks.poll_next(waiter, max) {
		Poll::Ready(result) => result,
		Poll::Pending => Err(moq_quic::recv::Error::Blocked),
	}
}

/// Copy from `chunks` until `buf` is full or nothing more is readable, returning how much.
///
/// It waits on `waiter` only when it read nothing.
fn fill(chunks: &mut Chunks<'_>, waiter: &kio::Waiter, buf: &mut impl BufMut) -> ReadStatus<usize> {
	let mut read = 0;
	loop {
		if !buf.has_remaining_mut() {
			// We know `read` is not 0 because `buf` had room before
			return ReadStatus::Readable(read);
		}

		let max = buf.remaining_mut();
		let next = match read {
			0 => poll_next(chunks, waiter, max),
			_ => chunks.next(max),
		};
		match next {
			Ok(Some(chunk)) => {
				buf.put_slice(&chunk.bytes);
				read += chunk.bytes.len();
			}
			res => return ((read > 0).then_some(read), res.err()).into(),
		}
	}
}

/// The error for a lease moq-quic refused, which forgets a stream only once the connection
/// closed.
fn lease_error(err: moq_quic::recv::Error, closed: Option<ConnectionError>) -> Error {
	match err {
		moq_quic::recv::Error::Reset(code) => Error::Reset(code),
		moq_quic::recv::Error::Evicted => Error::Evicted,
		moq_quic::recv::Error::TooLarge => Error::TooLarge,
		err => read_error(err, closed),
	}
}

/// The error for a read moq-quic refused on a stream the handle still holds, which it forgets
/// only once the connection closed.
fn read_error(err: moq_quic::recv::Error, closed: Option<ConnectionError>) -> Error {
	match closed {
		Some(closed) => Error::Connection(closed),
		None => panic!("a held receive stream failed to read: {err:?}"),
	}
}

/// Unreleased bytes of a [`Stream`] from [`Stream::poll_lease`], read as a [`Buf`]
///
/// It owns the bytes, so the connection carries on while it is held. Hand it back with
/// [`Self::release`] to return the credit of the bytes read; dropping it keeps their credit,
/// as for a message read only in part. Either way the bytes not read are leased again next
/// time.
#[derive(Debug)]
pub struct Lease<'a> {
	reader: &'a mut Reader,
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
		let mut conn = self.reader.conn.state.lock();
		let _ = conn.inner.recv_stream(self.reader.id).release(lease);
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
		if let Some(lease) = self.inner.take()
			&& let Some(mut conn) = self.reader.conn.state.lock_for_drop()
		{
			let _ = conn.inner.recv_stream(self.reader.id).restore(lease);
		}
	}
}

enum ReadStatus<T> {
	Readable(T),
	Finished(Option<T>),
	Failed(Option<T>, moq_quic::recv::Error),
}

impl<T> From<(Option<T>, Option<moq_quic::recv::Error>)> for ReadStatus<T> {
	fn from(status: (Option<T>, Option<moq_quic::recv::Error>)) -> Self {
		match status {
			(read, None) => Self::Finished(read),
			(read, Some(e)) => Self::Failed(read, e),
		}
	}
}

impl tokio::io::AsyncRead for Stream {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		let this = self.get_mut();
		let waiter = this.park.hold(cx);
		let mut unfilled = buf.initialize_unfilled();
		let read = ready!(this.reader.poll_read_buf(waiter, &mut unfilled))?;
		buf.advance(read.unwrap_or(0));
		Poll::Ready(Ok(()))
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		let reader = &self.reader;
		let Some(mut conn) = reader.conn.state.lock_for_drop() else {
			return;
		};
		if reader.all_data_read || conn.is_lost() {
			return;
		}

		// Fails harmlessly once the stream is gone.
		let _ = conn.inner.recv_stream(reader.id).stop(reader.drop_stop);
	}
}

/// Errors from reading or leasing a stream
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// The peer abandoned transmitting data on this stream
	///
	/// Carries an application-defined error code.
	#[error("stream reset by peer: error {0}")]
	Reset(VarInt),
	/// The connection window was stuck and this stream was discarded to free it
	///
	/// See [`Stream::poll_lease`]. The peer hears of it once the stream is stopped, so call
	/// [`Stream::stop`] with your own code: dropping the stream sends code 0.
	#[error("evicted")]
	Evicted,
	/// The stream finished before [`Stream::read_exact`] filled its buffer, after this many bytes
	#[error("stream finished early ({0} bytes read)")]
	FinishedEarly(usize),
	/// The stream is longer than [`Stream::read_to_end`]'s limit
	#[error("stream too long")]
	TooLong,
	/// A lease wanted more than the connection receive window can hold
	#[error("want exceeds the receive window")]
	TooLarge,
	/// The connection was lost
	#[error("connection lost: {0}")]
	Connection(#[from] ConnectionError),
}

impl From<Error> for io::Error {
	fn from(x: Error) -> Self {
		let kind = match x {
			Error::Reset(_) | Error::Evicted => io::ErrorKind::ConnectionReset,
			Error::FinishedEarly(_) => io::ErrorKind::UnexpectedEof,
			Error::TooLong | Error::TooLarge => io::ErrorKind::InvalidInput,
			Error::Connection(_) => io::ErrorKind::NotConnected,
		};
		Self::new(kind, x)
	}
}
