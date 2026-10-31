//! Outgoing streams and datagrams: their handles, scheduling, and errors.

use std::{
	future::{Future, poll_fn},
	io,
	pin::Pin,
	sync::Arc,
	task::{Context, Poll},
};

use bytes::Buf;
use moq_quic::{connection::Error as ConnectionError, send::End};
use thiserror::Error;

use crate::quic::driver::{
	VarInt,
	connection::{ConnectionInner, ConnectionRef, State},
	stream,
};

/// A stream that can only be used to send data
///
/// [`finish()`](Self::finish) it to end it cleanly, which hands back a [`Finished`] that can
/// no longer be written. Dropped unfinished, it is [`reset()`](Self::reset), with code 0 unless
/// the peer stopped it, so the peer can't mistake a truncated stream for a complete one.
///
/// # Cancellation
///
/// A `write` method is said to be *cancel-safe* when dropping its future before the future becomes
/// ready will always result in no data being written to the stream. This is true of methods which
/// succeed immediately when any progress is made, and is not true of methods which might need to
/// perform multiple writes internally before succeeding. Each `write` method documents whether it is
/// cancel-safe.
#[derive(Debug)]
pub struct Stream(Half);

/// A stream [`Stream::finish`]ed, which keeps (re)transmitting its data until the peer
/// acknowledges all of it or the connection closes
///
/// Dropping it leaves the stream to finish, unless the peer stopped it, in which case it is
/// reset with the peer's code.
#[derive(Debug)]
pub struct Finished(Half);

/// What [`Stream`] and [`Finished`] share.
#[derive(Debug)]
struct Half {
	conn: ConnectionRef,
	id: stream::Id,
	state: HalfState,
}

#[derive(Debug)]
enum HalfState {
	/// Still open: a drop resets at the offset with the code.
	Open(Option<Offset>, VarInt),
	/// Finished, by [`Stream::finish`] or an [`AsyncWrite`](tokio::io::AsyncWrite) shutdown.
	Finished,
	Reset,
}

impl Stream {
	pub(crate) fn new(conn: ConnectionRef, id: stream::Id) -> Self {
		Self(Half {
			conn,
			id,
			state: HalfState::Open(None, VarInt::from_u32(0)),
		})
	}

	/// Write from `buf` into this stream, returning how many bytes were written
	///
	/// Unless this method errors, it waits until some of `buf` can be written into this stream,
	/// and then writes as much as it can without waiting again, advancing `buf` past it. Due to
	/// congestion and flow control, this may be less than `buf.remaining()`. A [`Bytes`](bytes::Bytes)
	/// source is taken without copying.
	///
	/// # Cancel safety
	///
	/// This method is cancellation safe. If this does not resolve, no bytes were written.
	pub async fn write(&mut self, buf: &mut impl Buf) -> Result<usize, Error> {
		poll_fn(|cx| self.poll_write(cx, buf)).await
	}

	/// Write all of `buf` into this stream
	///
	/// # Cancel safety
	///
	/// This method is *not* cancellation safe. Even if this does not resolve, some prefix of `buf`
	/// may have been written when previously polled.
	pub async fn write_all(&mut self, mut buf: impl Buf) -> Result<(), Error> {
		while buf.has_remaining() {
			self.write(&mut buf).await?;
		}
		Ok(())
	}

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &mut impl Buf) -> Poll<Result<usize, Error>> {
		assert!(
			matches!(self.0.state, HalfState::Open(..)),
			"wrote to a stream after shutting it down"
		);
		let mut conn = self.0.conn.state.lock();
		if let Some(x) = &conn.error {
			return Poll::Ready(Err(Error::Connection(x.clone())));
		}
		match conn.inner.send_stream(self.0.id).write(buf) {
			Ok(written) => {
				conn.wake();
				Poll::Ready(Ok(written))
			}
			Err(moq_quic::send::Error::Blocked) => {
				conn.blocked_writers.insert(self.0.id, cx.waker().clone());
				Poll::Pending
			}
			Err(moq_quic::send::Error::Stopped(code)) => Poll::Ready(Err(Error::Stopped(code))),
			Err(err) => unreachable!("an open stream failed to write: {err:?}"),
		}
	}

	/// Notify the peer that no more data will ever be written to this stream
	///
	/// [`Finished::stopped()`] waits for the peer to receive all of it, or learns that it
	/// stopped the stream first.
	pub fn finish(mut self) -> Finished {
		self.0.finish();
		Finished(self.0)
	}

	/// Close the send stream immediately
	///
	/// Locally buffered data is dropped, and previously transmitted data will no longer be
	/// retransmitted if lost.
	pub fn reset(mut self, error_code: VarInt) {
		self.0.reset(None, error_code);
	}

	/// Close the send stream immediately, but still deliver the data before `offset`
	///
	/// Like [`Self::reset()`], except the data before `offset` still reaches the peer. A peer
	/// that accepts RESET_STREAM_AT reads it before the error. Otherwise the reset waits a round
	/// trip until the peer acknowledges the data, and the peer's application may never read it:
	/// see [`moq_quic::send::Stream::reset_at`].
	///
	/// # Panics
	/// - when `offset` came from another stream
	pub fn reset_at(mut self, offset: Offset, error_code: VarInt) {
		self.0.reset(Some(offset), error_code);
	}

	/// The offset just past the data written so far
	pub fn offset(&self) -> Offset {
		let mut conn = self.0.conn.state.lock();
		Offset {
			inner: conn.inner.send_stream(self.0.id).offset(),
			conn: self.0.conn.stable_id(),
			id: self.0.id,
		}
	}

	/// Reset at `offset` with `code`, instead of at 0 with code 0, if dropped before `finish`
	#[cfg(feature = "quic")]
	pub(crate) fn set_drop_reset(&mut self, offset: Option<Offset>, code: VarInt) {
		if let Some(offset) = &offset {
			self.0.own(offset);
		}
		self.0.state = HalfState::Open(offset, code);
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped
	///
	/// Every send stream has an initial priority of 0. Locally buffered data from streams with
	/// higher priority will be transmitted before data from streams with lower priority. Changing
	/// the priority of a stream with pending data may only take effect after that data has been
	/// transmitted. Using many different priority levels per connection may have a negative
	/// impact on performance.
	pub fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority);
	}

	/// The priority of the send stream
	pub fn priority(&self) -> i64 {
		self.0.priority()
	}

	/// Move the stream into a send group, or with `None` back to the ungrouped streams, which
	/// go before every group
	///
	/// # Panics
	/// - when `group` belongs to another connection
	pub fn set_group(&mut self, group: Option<&Group>) {
		self.0.set_group(group);
	}

	/// Completes when the peer stops the stream, or the stream ends otherwise
	///
	/// See [`Finished::stopped()`]. A stream reset or dropped before the peer stopped it
	/// resolves with `None`.
	pub fn stopped(&self) -> impl Future<Output = Result<Option<VarInt>, Error>> + Send + Sync + 'static {
		self.0.stopped()
	}

	/// The identity of this stream
	pub fn id(&self) -> stream::Id {
		self.0.id
	}
}

impl Finished {
	/// Abandon the stream's data the peer has not acknowledged yet. See [`Stream::reset()`].
	pub fn reset(mut self, error_code: VarInt) {
		self.0.reset(None, error_code);
	}

	/// Abandon the stream's data past `offset`. See [`Stream::reset_at()`].
	///
	/// # Panics
	/// - when `offset` came from another stream
	pub fn reset_at(mut self, offset: Offset, error_code: VarInt) {
		self.0.reset(Some(offset), error_code);
	}

	/// Set the priority its data is retransmitted at. See [`Stream::set_priority()`].
	pub fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority);
	}

	/// The priority of the send stream
	pub fn priority(&self) -> i64 {
		self.0.priority()
	}

	/// Move the stream into a send group. See [`Stream::set_group()`].
	///
	/// # Panics
	/// - when `group` belongs to another connection
	pub fn set_group(&mut self, group: Option<&Group>) {
		self.0.set_group(group);
	}

	/// Completes when the peer stops the stream or acknowledges all of its data
	///
	/// Yields `Some` with the stop error code if the peer stops the stream. Yields `None` once
	/// the peer acknowledges receipt of all stream data (although not necessarily the processing
	/// of it), or once the stream is reset, after which the peer closing it is no longer
	/// meaningful. A peer's stop code still wins over a local reset if it arrived first, but not
	/// if it arrives after.
	///
	/// For a variety of reasons, the peer may not send acknowledgements immediately upon receiving
	/// data. As such, relying on `stopped` to know when the peer has read a stream to completion
	/// may introduce more latency than using an application-level response of some sort.
	pub fn stopped(&self) -> impl Future<Output = Result<Option<VarInt>, Error>> + Send + Sync + 'static {
		self.0.stopped()
	}

	/// The identity of this stream
	pub fn id(&self) -> stream::Id {
		self.0.id
	}
}

impl Half {
	/// Check `offset` before taking the lock: moq-quic makes the same check, but panicking
	/// there poisons the lock, so every handle's drop panics again and aborts the process.
	fn own(&self, offset: &Offset) {
		assert!(
			offset.conn == self.conn.stable_id() && offset.id == self.id,
			"offset used on another stream"
		);
	}

	fn finish(&mut self) {
		let mut conn = self.conn.state.lock();
		// A STOP_SENDING may have beaten us here, which `stopped` reports.
		if conn.inner.send_stream(self.id).finish().is_ok() {
			conn.wake();
		}
		self.state = HalfState::Finished;
	}

	fn reset(&mut self, offset: Option<Offset>, code: VarInt) {
		if let Some(offset) = &offset {
			self.own(offset);
		}
		self.state = HalfState::Reset;
		let mut conn = self.conn.state.lock();
		let mut stream = conn.inner.send_stream(self.id);
		let stopped = match stream.end() {
			Ok(Some(End::Stopped(code))) => Some(code),
			_ => None,
		};
		let reset = match offset {
			Some(offset) => stream.reset_at(offset.inner, code),
			None => stream.reset(code),
		};
		// Fails harmlessly once the connection is gone.
		if reset.is_ok() {
			conn.resolve_stopped(self.id, stopped);
			conn.wake();
		}
	}

	fn set_priority(&mut self, priority: i64) {
		let mut conn = self.conn.state.lock();
		let _ = conn.inner.send_stream(self.id).set_priority(priority);
	}

	fn priority(&self) -> i64 {
		let mut conn = self.conn.state.lock();
		conn.inner.send_stream(self.id).priority().unwrap_or_default()
	}

	fn set_group(&mut self, group: Option<&Group>) {
		let group = group.map(|group| group.id_on(&self.conn.0));
		let mut conn = self.conn.state.lock();
		let _ = conn.inner.send_stream(self.id).set_group(group);
	}

	fn stopped(&self) -> impl Future<Output = Result<Option<VarInt>, Error>> + Send + Sync + 'static {
		let conn = self.conn.clone();
		let id = self.id;
		// Join the waiters now rather than on the first poll, so a reset in between, which no
		// stream event reports, still reaches this future.
		let waiters = {
			let mut conn = conn.state.lock();
			match ended(&mut conn, id) {
				Some(output) => Err(output),
				None => Ok(conn.stopped.entry(id).or_default().clone()),
			}
		};
		async move {
			let mut waiters = match waiters {
				Ok(waiters) => waiters,
				Err(output) => return output,
			};
			loop {
				// The `Notify::notified` future needs to be created while the lock is being held,
				// otherwise a wakeup could be missed if triggered inbetween releasing the lock
				// and creating the future.
				// The lock may only be held in a block without `await`s, otherwise the future
				// becomes `!Send`. `Notify::notified` is lifetime-bound to `Notify`, therefore
				// we need to declare `waiters` outside of the block, and update it inside.
				{
					let mut conn = conn.state.lock();
					if let Some(&stopped) = waiters.outcome.get() {
						return Ok(stopped);
					}
					if let Some(output) = ended(&mut conn, id) {
						return output;
					}

					// A wakeup that resolved nothing removed the waiters; join the current ones
					waiters = conn.stopped.entry(id).or_default().clone();
					waiters.notify.notified()
				}
				.await
			}
		}
	}
}

/// How a send stream ended, or `None` while it goes on
///
/// A released stream's end is gone, but its handle settled the waiters when it let go.
fn ended(conn: &mut State, id: stream::Id) -> Option<Result<Option<VarInt>, Error>> {
	match conn.inner.send_stream(id).end() {
		Ok(Some(End::Stopped(code))) => Some(Ok(Some(code))),
		Ok(Some(_)) => Some(Ok(None)),
		Ok(None) | Err(_) => conn.error.clone().map(|error| Err(error.into())),
	}
}

impl Drop for Half {
	fn drop(&mut self) {
		let mut conn = self.conn.state.lock();

		// clean up any previously registered wakers
		conn.blocked_writers.remove(&self.id);

		if conn.error.is_none() {
			let mut stream = conn.inner.send_stream(self.id);
			let stopped = match stream.end() {
				Ok(Some(End::Stopped(code))) => Some(code),
				_ => None,
			};
			let reset = match (&self.state, stopped) {
				(HalfState::Reset, _) => None,
				// Echo the code, as RFC 9000 section 3.5 asks, even after `finish`: the peer reads
				// nothing more, so retransmitting the rest is wasted.
				(_, Some(code)) => Some(stream.reset(code)),
				(HalfState::Finished, None) => None,
				(HalfState::Open(offset, code), None) => Some(match offset {
					Some(offset) => stream.reset_at(offset.inner, *code),
					None => stream.reset(*code),
				}),
			};
			if reset.is_some_and(|reset| reset.is_ok()) {
				conn.resolve_stopped(self.id, stopped);
				conn.wake();
			}
		}
		conn.inner.send_stream(self.id).release();
	}
}

impl tokio::io::AsyncWrite for Stream {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, mut buf: &[u8]) -> Poll<io::Result<usize>> {
		let this = self.get_mut();
		if !matches!(this.0.state, HalfState::Open(..)) {
			return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
		}
		this.poll_write(cx, &mut buf).map_err(Into::into)
	}

	fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		Poll::Ready(Ok(()))
	}

	fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		let this = self.get_mut();
		if matches!(this.0.state, HalfState::Open(..)) {
			this.0.finish();
		}
		Poll::Ready(Ok(()))
	}
}

/// A position in a [`Stream`]'s data, minted by [`Stream::offset()`] and good only on that
/// stream, so it is never past the data written
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offset {
	inner: moq_quic::send::Offset,
	/// The connection's [`ConnectionRef::stable_id`], checked before taking its lock.
	conn: usize,
	id: stream::Id,
}

impl From<Offset> for u64 {
	fn from(offset: Offset) -> Self {
		offset.inner.into()
	}
}

/// How an outgoing stream or datagram is scheduled: its [`Group`] and priority
#[derive(Debug, Default, Clone, Copy)]
pub struct Options<'a> {
	/// The group it joins, or `None` to go before every group
	pub group: Option<&'a Group>,
	/// Its priority within the group, or among the ungrouped streams and datagrams
	pub priority: i64,
}

impl<'a> Options<'a> {
	/// The options as `conn` knows them.
	///
	/// # Panics
	/// - when the group belongs to another connection
	pub(crate) fn to_quic(self, conn: &ConnectionInner) -> moq_quic::send::Options<'a> {
		moq_quic::send::Options {
			group: self.group.map(|group| group.id_on(conn)),
			priority: self.priority,
		}
	}
}

/// A send group on a [`Connection`](crate::quic::driver::connection::Connection)
///
/// Streams that [join](Stream::set_group) a group share bytes with the connection's other
/// groups: a higher priority group preempts every lower one, and groups of equal priority get an
/// equal share of bytes however many streams each has queued. A stream's own priority orders it
/// only against the group's other streams, and a datagram sent in the group is ordered among
/// them by its priority. Streams and datagrams outside any group go before every group. See
/// [`moq_quic::send::Groups`].
///
/// Clones share one group, released once the last clone is dropped and its streams finish
/// and its datagrams are sent. A group does not keep its connection open.
#[derive(Debug, Clone)]
pub struct Group(Arc<GroupInner>);

#[derive(Debug)]
struct GroupInner {
	conn: Arc<ConnectionInner>,
	group: Option<moq_quic::send::Group>,
}

impl Group {
	pub(crate) fn new(conn: Arc<ConnectionInner>, priority: i64) -> Self {
		let group = conn.state.lock().inner.send_groups().open(priority);
		Self(Arc::new(GroupInner {
			conn,
			group: Some(group),
		}))
	}

	/// Change the group's priority, moving any data it has queued along with it
	pub fn set_priority(&self, priority: i64) {
		let mut conn = self.0.conn.state.lock();
		conn.inner.send_groups().set_priority(self.id(), priority);
	}

	/// The group's priority
	pub fn priority(&self) -> i64 {
		let mut conn = self.0.conn.state.lock();
		conn.inner.send_groups().priority(self.id())
	}

	/// The group as the connection knows it, which refuses it on any other connection.
	fn id(&self) -> &moq_quic::send::Group {
		self.0.group.as_ref().expect("only taken on drop")
	}

	/// The group as `conn` knows it.
	///
	/// Call it before taking `conn`'s lock: moq-quic makes the same check, but panicking
	/// there poisons the lock, so every handle's drop panics again and aborts the process.
	///
	/// # Panics
	/// - when the group belongs to another connection
	pub(crate) fn id_on(&self, conn: &ConnectionInner) -> &moq_quic::send::Group {
		assert!(
			std::ptr::eq(&*self.0.conn, conn),
			"send group used on another connection"
		);
		self.id()
	}
}

impl Drop for GroupInner {
	fn drop(&mut self) {
		if let Some(group) = self.group.take() {
			let mut conn = self.conn.state.lock();
			conn.inner.send_groups().close(group);
		}
	}
}

/// Errors from sending on a stream or as a datagram
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
	/// The peer is no longer accepting data on this stream
	///
	/// Carries an application-defined error code.
	#[error("sending stopped by peer: error {0}")]
	Stopped(VarInt),
	/// The datagram is larger than the connection can send
	#[error("datagram too large")]
	TooLarge,
	/// Datagrams are disabled locally or unsupported by the peer
	#[error("datagrams unsupported")]
	Unsupported,
	/// The connection was lost
	#[error("connection lost: {0}")]
	Connection(#[from] ConnectionError),
}

impl From<Error> for io::Error {
	fn from(x: Error) -> Self {
		let kind = match x {
			Error::Stopped(_) => io::ErrorKind::ConnectionReset,
			Error::TooLarge => io::ErrorKind::InvalidInput,
			Error::Unsupported => io::ErrorKind::Unsupported,
			Error::Connection(_) => io::ErrorKind::NotConnected,
		};
		Self::new(kind, x)
	}
}
