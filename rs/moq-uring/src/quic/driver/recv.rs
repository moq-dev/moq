//! Incoming streams: their handles, leases, and errors.
//!
//! A read comes straight out of moq-quic's reassembly buffer; the handles kick the driver when
//! a read owes the peer credit and park on the per-stream waiter lists the driver wakes.

use std::future::poll_fn;
use std::task::{Context, Poll, ready};

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
	/// Retains the waiter registrations of the polls across calls.
	park: kio::Park,
	all_data_read: bool,
	reset: Option<VarInt>,
	/// The code a drop before the end stops with
	drop_stop: VarInt,
}

impl Stream {
	pub(crate) fn new(shared: Shared, id: stream::Id) -> Self {
		Self {
			shared,
			id,
			park: kio::Park::default(),
			all_data_read: false,
			reset: None,
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
		poll_fn(|cx| self.poll_read(cx, buf)).await
	}

	/// [`Self::read`], as a poll.
	pub(crate) fn poll_read(&mut self, cx: &Context<'_>, buf: &mut impl BufMut) -> Poll<Result<Option<usize>, Error>> {
		if !buf.has_remaining_mut() {
			return Poll::Ready(Ok(Some(0)));
		}
		self.parked(cx, |this, waiter| {
			this.poll_read_generic(waiter, |chunks| fill(chunks, buf))
		})
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
	pub async fn read_chunk(&mut self, max: usize) -> Result<Option<Bytes>, Error> {
		poll_fn(|cx| self.poll_read_chunk(cx, max)).await
	}

	/// [`Self::read_chunk`], as a poll.
	pub(crate) fn poll_read_chunk(&mut self, cx: &Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		self.parked(cx, |this, waiter| {
			this.poll_read_generic(waiter, |chunks| match chunks.next(max) {
				Ok(Some(chunk)) => ReadStatus::Readable(chunk.bytes),
				res => (None, res.err()).into(),
			})
		})
	}

	/// Run `poll` with a waiter held in [`Self::park`], which outlives the call.
	fn parked<R>(&mut self, cx: &Context<'_>, poll: impl FnOnce(&mut Self, &kio::Waiter) -> R) -> R {
		let mut park = std::mem::take(&mut self.park);
		let result = poll(self, park.hold(cx));
		self.park = park;
		result
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
		let inner = poll_fn(|cx| self.parked(cx, |this, waiter| this.poll_take(waiter, want))).await?;
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
	pub fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		let inner = ready!(self.parked(cx, |this, waiter| this.poll_take(waiter, want)))?;
		Poll::Ready(Ok(Lease {
			stream: self,
			inner: Some(inner),
		}))
	}

	/// Take the connection's lease, which [`Lease`] wraps once this returns: the borrow of `self`
	/// it holds cannot start inside a poll that may return `Pending`.
	fn poll_take(&mut self, waiter: &kio::Waiter, want: usize) -> Poll<Result<moq_quic::recv::Lease, Error>> {
		// moq-quic freed the stream when it reported the reset.
		if let Some(code) = self.reset {
			return Poll::Ready(Err(Error::Reset(code)));
		}
		let (result, transmit) = self.shared.conn.borrow_mut().recv_stream(self.id).lease(want);
		if transmit.should_transmit() {
			self.shared.kick();
		}
		let err = match result {
			Ok(lease) => return Poll::Ready(Ok(lease)),
			Err(moq_quic::recv::Error::Blocked) => {
				if let Some(err) = self.shared.closed() {
					return Poll::Ready(Err(Error::Connection(err)));
				}
				self.shared.park_readable(self.id, waiter);
				return Poll::Pending;
			}
			Err(moq_quic::recv::Error::Reset(code)) => Error::Reset(code),
			Err(moq_quic::recv::Error::Evicted) => Error::Evicted,
			Err(moq_quic::recv::Error::TooLarge) => Error::TooLarge,
			Err(err) => self.read_error(err),
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

	/// Completes when the stream has been reset by the peer or otherwise closed.
	///
	/// Yields `Some` with the reset error code when the stream is reset by the peer, and `None`
	/// once every byte the peer finished it with has been read, after which it is no longer
	/// meaningful for the stream to be reset.
	///
	/// Cancel-safe.
	pub async fn received_reset(&mut self) -> Result<Option<VarInt>, Error> {
		poll_fn(|cx| self.parked(cx, Self::wait_received_reset)).await
	}

	fn wait_received_reset(&mut self, waiter: &kio::Waiter) -> Poll<Result<Option<VarInt>, Error>> {
		if let Some(code) = self.reset {
			return Poll::Ready(Ok(Some(code)));
		}
		let reset = self.shared.conn.borrow_mut().recv_stream(self.id).received_reset();
		match reset {
			Err(_) => Poll::Ready(Ok(None)),
			Ok(Some(code)) => {
				// The stream's state was just freed, so the peer may be owed stream credit, and
				// later reads fail with the cached code.
				self.reset = Some(code);
				self.shared.kick();
				Poll::Ready(Ok(Some(code)))
			}
			Ok(None) => match self.shared.closed() {
				Some(err) => Poll::Ready(Err(Error::Connection(err))),
				// A reset is a read error, so it always wakes readers.
				None => {
					self.shared.park_readable(self.id, waiter);
					Poll::Pending
				}
			},
		}
	}

	/// Completes once the stream ends: `None` once its finish is read, or `Some` with a reset's
	/// code.
	///
	/// Unlike [`Self::received_reset`], it observes a finish whose bytes were all taken but
	/// whose end was not read yet. An empty read reaches the end without taking the bytes
	/// before it, so it keeps waiting while any are unread.
	pub(crate) fn poll_closed(&mut self, cx: &Context<'_>) -> Poll<Result<Option<VarInt>, Error>> {
		if let Some(code) = self.reset {
			return Poll::Ready(Ok(Some(code)));
		}
		let read = self.parked(cx, |this, waiter| {
			this.poll_read_generic(waiter, |chunks| match chunks.next(0) {
				// Bytes are still unread, so the end is not reached.
				Ok(Some(_)) => ReadStatus::Failed(None, moq_quic::recv::Error::Blocked),
				res => (None::<()>, res.err()).into(),
			})
		});
		Poll::Ready(match ready!(read) {
			Ok(_) => Ok(None),
			Err(Error::Reset(code)) => Ok(Some(code)),
			Err(err) => Err(err),
		})
	}

	/// Handle the logic common to every read out of a receive stream.
	///
	/// `read_fn` reads from the passed `Chunks` and returns the status after reading: the
	/// amount of data read, and the status after the final read call.
	fn poll_read_generic<T, U>(&mut self, waiter: &kio::Waiter, mut read_fn: T) -> Poll<Result<Option<U>, Error>>
	where
		T: FnMut(&mut Chunks<'_>) -> ReadStatus<U>,
	{
		use moq_quic::recv::Error::{Blocked, Reset};

		if self.all_data_read {
			return Poll::Ready(Ok(None));
		}

		// A stored reset is returned now: a `read_fn` can both return data and end in a reset.
		let status = match self.reset {
			Some(code) => ReadStatus::Failed(None, Reset(code)),
			None => {
				let mut conn = self.shared.conn.borrow_mut();
				let (status, transmit) = match conn.recv_stream(self.id).read() {
					Ok(mut chunks) => {
						let status = read_fn(&mut chunks);
						(status, chunks.finalize().should_transmit())
					}
					Err(err) => (ReadStatus::Failed(None, err), false),
				};
				drop(conn);
				// Reading is what returns the peer's flow control credit.
				if transmit {
					self.shared.kick();
				}
				status
			}
		};

		match status {
			ReadStatus::Readable(read) => Poll::Ready(Ok(Some(read))),
			ReadStatus::Finished(read) => {
				self.all_data_read = true;
				Poll::Ready(Ok(read))
			}
			ReadStatus::Failed(Some(read), Blocked) => Poll::Ready(Ok(Some(read))),
			ReadStatus::Failed(None, Blocked) => {
				if let Some(err) = self.shared.closed() {
					return Poll::Ready(Err(Error::Connection(err)));
				}
				self.shared.park_readable(self.id, waiter);
				Poll::Pending
			}
			ReadStatus::Failed(read, Reset(code)) => {
				self.reset = Some(code);
				if read.is_some() {
					return Poll::Ready(Ok(read));
				}
				self.all_data_read = true;
				Poll::Ready(Err(Error::Reset(code)))
			}
			ReadStatus::Failed(_, err) => Poll::Ready(Err(self.read_error(err))),
		}
	}

	/// The error for a read moq-quic refused on a stream the handle still holds, which it forgets
	/// only once the connection closed.
	fn read_error(&self, err: moq_quic::recv::Error) -> Error {
		match self.shared.closed() {
			Some(closed) => Error::Connection(closed),
			None => panic!("a held receive stream failed to read: {err:?}"),
		}
	}
}

/// Copy from `chunks` until `buf` is full or nothing more is readable, returning how much.
fn fill(chunks: &mut Chunks<'_>, buf: &mut impl BufMut) -> ReadStatus<usize> {
	let mut read = 0;
	loop {
		if !buf.has_remaining_mut() {
			// `read` is not 0, because `buf` had room before.
			return ReadStatus::Readable(read);
		}

		match chunks.next(buf.remaining_mut()) {
			Ok(Some(chunk)) => {
				buf.put_slice(&chunk.bytes);
				read += chunk.bytes.len();
			}
			res => return ((read > 0).then_some(read), res.err()).into(),
		}
	}
}

impl Drop for Stream {
	fn drop(&mut self) {
		self.shared.forget_recv(self.id);
		if self.all_data_read || self.shared.closed().is_some() {
			return;
		}
		// Fails harmlessly once the stream is gone.
		let _ = self.shared.conn.borrow_mut().recv_stream(self.id).stop(self.drop_stop);
		self.shared.kick();
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
		let transmit = stream.shared.conn.borrow_mut().recv_stream(stream.id).release(lease);
		if transmit.should_transmit() {
			stream.shared.kick();
		}
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
			let transmit = stream.shared.conn.borrow_mut().recv_stream(stream.id).restore(lease);
			if transmit.should_transmit() {
				stream.shared.kick();
			}
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

/// What the `web_transport_trait` impls report, which have one error type per session.
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
