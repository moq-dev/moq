//! Outgoing streams and datagrams: their handles, scheduling, and errors.
//!
//! Single-threaded sans-IO means a write goes straight into moq-quic's send queue (no staging
//! copy); the handles just kick the driver so egress reaches the wire and park on the
//! per-stream waiter lists the driver wakes.

use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Context, Poll};

use bytes::Buf;
use moq_quic::send::End;

use super::Shared;
use crate::quic::{Error as ConnectionError, VarInt, stream};

/// A stream that can only be used to send data.
///
/// [`finish()`](Self::finish) it to end it cleanly, which hands back a [`Finished`] that can no
/// longer be written. Dropped unfinished, it is [`reset()`](Self::reset), with code 0 unless the
/// peer stopped it, so the peer can't mistake a truncated stream for a complete one.
pub struct Stream(Half);

/// A stream [`Stream::finish`]ed, which keeps (re)transmitting its data until the peer
/// acknowledges all of it or the connection closes.
///
/// Dropping it leaves the stream to finish, unless the peer stopped it, in which case it is
/// reset with the peer's code.
pub struct Finished(Half);

/// A position in a [`Stream`]'s data, minted by [`Stream::offset()`] and good only on that
/// stream, so it is never past the data written.
pub use moq_quic::send::Offset;

/// What [`Stream`] and [`Finished`] share.
struct Half {
	shared: Shared,
	id: stream::Id,
	/// Retains the waiter registrations of the polls across calls.
	park: kio::Park,
	state: HalfState,
}

enum HalfState {
	/// Still open: a drop resets at the offset with the code.
	Open(Option<Offset>, VarInt),
	Finished,
	Reset,
}

impl Stream {
	pub(crate) fn new(shared: Shared, id: stream::Id) -> Self {
		Self(Half {
			shared,
			id,
			park: kio::Park::default(),
			state: HalfState::Open(None, VarInt::from_u32(0)),
		})
	}

	/// Write from `buf` into this stream, returning how many bytes were written.
	///
	/// Waits until some of `buf` can be written, then writes as much as it can without waiting
	/// again, advancing `buf` past it. Due to congestion and flow control, this may be less than
	/// `buf.remaining()`. A [`Bytes`](bytes::Bytes) source is taken without copying.
	///
	/// Cancel-safe: if this does not resolve, no bytes were written.
	pub async fn write(&mut self, buf: &mut impl Buf) -> Result<usize, Error> {
		poll_fn(|cx| self.poll_write(cx, buf)).await
	}

	/// Write all of `buf` into this stream.
	///
	/// Not cancel-safe: some prefix of `buf` may have been written when previously polled.
	pub async fn write_all(&mut self, mut buf: impl Buf) -> Result<(), Error> {
		while buf.has_remaining() {
			self.write(&mut buf).await?;
		}
		Ok(())
	}

	/// [`Self::write`], as a poll.
	pub(crate) fn poll_write(&mut self, cx: &Context<'_>, buf: &mut impl Buf) -> Poll<Result<usize, Error>> {
		let waiter = self.0.park.hold(cx);
		let id = self.0.id;
		let shared = &self.0.shared;
		if let Some(err) = shared.closed() {
			return Poll::Ready(Err(Error::Connection(err)));
		}
		let written = shared.conn.borrow_mut().send_stream(id).write(buf);
		match written {
			Ok(written) => {
				shared.kick();
				Poll::Ready(Ok(written))
			}
			// The driver wakes us when moq-quic reports the stream writable, or stopped.
			Err(moq_quic::send::Error::Blocked) => {
				shared.park_writable(id, waiter);
				Poll::Pending
			}
			Err(moq_quic::send::Error::Stopped(code)) => Poll::Ready(Err(Error::Stopped(code))),
			Err(err) => unreachable!("an open stream failed to write: {err:?}"),
		}
	}

	/// Notify the peer that no more data will ever be written to this stream.
	///
	/// [`Finished::stopped()`] waits for the peer to receive all of it, or learns that it stopped
	/// the stream first.
	pub fn finish(mut self) -> Finished {
		self.0.finish();
		Finished(self.0)
	}

	/// Close the send stream immediately.
	///
	/// Locally buffered data is dropped, and previously transmitted data will no longer be
	/// retransmitted if lost.
	pub fn reset(mut self, error_code: VarInt) {
		self.0.reset(None, error_code);
	}

	/// Close the send stream immediately, but still deliver the data before `offset`.
	///
	/// See [`moq_quic::send::Stream::reset_at`].
	///
	/// # Panics
	/// - when `offset` came from another stream
	pub fn reset_at(mut self, offset: Offset, error_code: VarInt) {
		self.0.reset(Some(offset), error_code);
	}

	/// The offset just past the data written so far.
	pub fn offset(&self) -> Offset {
		self.0.shared.conn.borrow_mut().send_stream(self.0.id).offset()
	}

	/// Reset at `offset` with `code`, instead of at 0 with code 0, if dropped before `finish`.
	pub(crate) fn set_drop_reset(&mut self, offset: Option<Offset>, code: VarInt) {
		self.0.state = HalfState::Open(offset, code);
	}

	/// The peer's STOP_SENDING code, if it already stopped the stream.
	pub(crate) fn stop_code(&self) -> Option<VarInt> {
		match self.0.shared.conn.borrow_mut().send_stream(self.0.id).end() {
			Ok(Some(End::Stopped(code))) => Some(code),
			_ => None,
		}
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	///
	/// Higher goes first.
	pub fn set_priority(&mut self, priority: i64) {
		self.0.set_priority(priority);
	}

	/// The priority of the send stream.
	pub fn priority(&self) -> i64 {
		self.0.priority()
	}

	/// Move the stream into a send group, or with `None` back to the ungrouped streams, which
	/// go before every group.
	///
	/// # Panics
	/// - when `group` belongs to another connection
	pub fn set_group(&mut self, group: Option<&Group>) {
		self.0.set_group(group);
	}

	/// Completes when the peer stops the stream. See [`Finished::stopped()`].
	pub async fn stopped(&self) -> Result<Option<VarInt>, Error> {
		kio::wait(|waiter| poll_stopped(&self.0.shared, self.0.id, waiter)).await
	}

	/// [`Self::stopped`], as a poll.
	pub(crate) fn poll_stopped(&mut self, cx: &Context<'_>) -> Poll<Result<Option<VarInt>, Error>> {
		poll_stopped(&self.0.shared, self.0.id, self.0.park.hold(cx))
	}

	/// The identity of this stream.
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

	/// The priority of the send stream.
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

	/// Completes when the peer stops the stream or acknowledges all of its data.
	///
	/// Yields `Some` with the stop error code if the peer stops the stream, and `None` once the
	/// peer acknowledges receipt of all stream data (although not necessarily the processing of
	/// it). Fails only when the connection ends first, since then the caller cannot read success
	/// as "every byte arrived".
	pub async fn stopped(&self) -> Result<Option<VarInt>, Error> {
		kio::wait(|waiter| poll_stopped(&self.0.shared, self.0.id, waiter)).await
	}

	/// [`Self::stopped`], as a poll.
	pub(crate) fn poll_stopped(&mut self, cx: &Context<'_>) -> Poll<Result<Option<VarInt>, Error>> {
		poll_stopped(&self.0.shared, self.0.id, self.0.park.hold(cx))
	}

	/// The identity of this stream.
	pub fn id(&self) -> stream::Id {
		self.0.id
	}
}

impl Half {
	fn finish(&mut self) {
		// A STOP_SENDING may have beaten us here, which `stopped` reports.
		if self.shared.conn.borrow_mut().send_stream(self.id).finish().is_ok() {
			self.shared.kick();
		}
		self.state = HalfState::Finished;
	}

	fn reset(&mut self, offset: Option<Offset>, code: VarInt) {
		self.state = HalfState::Reset;
		let mut conn = self.shared.conn.borrow_mut();
		let mut stream = conn.send_stream(self.id);
		// Fails harmlessly once the connection is gone.
		let reset = match offset {
			Some(offset) => stream.reset_at(offset, code),
			None => stream.reset(code),
		};
		drop(conn);
		if reset.is_ok() {
			self.shared.kick();
		}
	}

	fn set_priority(&mut self, priority: i64) {
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.send_stream(self.id)
			.set_priority(priority);
	}

	fn priority(&self) -> i64 {
		self.shared
			.conn
			.borrow_mut()
			.send_stream(self.id)
			.priority()
			.unwrap_or_default()
	}

	fn set_group(&mut self, group: Option<&Group>) {
		let group = group.map(Group::id);
		let _ = self.shared.conn.borrow_mut().send_stream(self.id).set_group(group);
	}
}

/// Wait for send stream `id` to end: moq-quic keeps how it ended until its handle releases it.
fn poll_stopped(shared: &Shared, id: stream::Id, waiter: &kio::Waiter) -> Poll<Result<Option<VarInt>, Error>> {
	let end = shared.conn.borrow_mut().send_stream(id).end();
	match end {
		Ok(Some(End::Stopped(code))) => Poll::Ready(Ok(Some(code))),
		Ok(Some(_)) => Poll::Ready(Ok(None)),
		Ok(None) | Err(_) => match shared.closed() {
			Some(err) => Poll::Ready(Err(Error::Connection(err))),
			None => {
				shared.park_finishing(id, waiter);
				Poll::Pending
			}
		},
	}
}

impl Drop for Half {
	fn drop(&mut self) {
		self.shared.forget_send(self.id);
		let mut conn = self.shared.conn.borrow_mut();
		let mut stream = conn.send_stream(self.id);
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
				Some(offset) => stream.reset_at(*offset, *code),
				None => stream.reset(*code),
			}),
		};
		stream.release();
		drop(conn);
		if reset.is_some_and(|reset| reset.is_ok()) {
			self.shared.kick();
		}
	}
}

impl std::fmt::Debug for Stream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Stream").field("id", &self.0.id).finish()
	}
}

impl std::fmt::Debug for Finished {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Finished").field("id", &self.0.id).finish()
	}
}

/// How an outgoing stream or datagram is scheduled: its [`Group`] and priority.
#[derive(Debug, Default, Clone, Copy)]
pub struct Options<'a> {
	/// The group it joins, or `None` to go before every group
	pub group: Option<&'a Group>,
	/// Its priority within the group, or among the ungrouped streams and datagrams
	pub priority: i64,
}

impl<'a> Options<'a> {
	/// The options as the connection knows them.
	pub(crate) fn to_quic(self) -> moq_quic::send::Options<'a> {
		moq_quic::send::Options {
			group: self.group.map(Group::id),
			priority: self.priority,
		}
	}
}

/// A send group on a [`Connection`](super::Connection).
///
/// Streams that [join](Stream::set_group) a group share bytes with the connection's other
/// groups: a higher priority group preempts every lower one, and groups of equal priority get an
/// equal share of bytes however many streams each has queued. A stream's own priority orders it
/// only against the group's other streams, and a datagram sent in the group is ordered among
/// them by its priority. Streams and datagrams outside any group go before every group. See
/// [`moq_quic::send::Groups`].
///
/// Clones share one group, released once the last clone is dropped and its streams finish
/// and its datagrams are sent.
#[derive(Clone)]
pub struct Group(Rc<GroupInner>);

struct GroupInner {
	shared: Shared,
	group: Option<moq_quic::send::Group>,
}

impl Group {
	pub(crate) fn new(shared: Shared, priority: i64) -> Self {
		let group = shared.conn.borrow_mut().send_groups().open(priority);
		Self(Rc::new(GroupInner {
			shared,
			group: Some(group),
		}))
	}

	/// Change the group's priority, moving any data it has queued along with it.
	pub fn set_priority(&self, priority: i64) {
		self.0
			.shared
			.conn
			.borrow_mut()
			.send_groups()
			.set_priority(self.id(), priority);
	}

	/// The group's priority.
	pub fn priority(&self) -> i64 {
		self.0.shared.conn.borrow_mut().send_groups().priority(self.id())
	}

	/// The group as the connection knows it, which refuses it on any other connection.
	fn id(&self) -> &moq_quic::send::Group {
		self.0.group.as_ref().expect("only taken on drop")
	}
}

impl Drop for GroupInner {
	fn drop(&mut self) {
		if let Some(group) = self.group.take() {
			self.shared.conn.borrow_mut().send_groups().close(group);
		}
	}
}

impl std::fmt::Debug for Group {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Group").finish_non_exhaustive()
	}
}

/// Errors from sending on a stream or as a datagram.
#[derive(Debug, thiserror::Error, Clone)]
#[non_exhaustive]
pub enum Error {
	/// The peer is no longer accepting data on this stream, with its application error code.
	#[error("sending stopped by peer: error {0}")]
	Stopped(VarInt),
	/// The datagram is larger than the connection can send.
	#[error("datagram too large")]
	TooLarge,
	/// Datagrams are disabled locally or unsupported by the peer.
	#[error("datagrams unsupported")]
	Unsupported,
	/// The connection was lost.
	#[error("connection lost: {0}")]
	Connection(#[from] ConnectionError),
}

impl From<Error> for std::io::Error {
	fn from(x: Error) -> Self {
		let kind = match x {
			Error::Stopped(_) => std::io::ErrorKind::ConnectionReset,
			Error::TooLarge => std::io::ErrorKind::InvalidInput,
			Error::Unsupported => std::io::ErrorKind::Unsupported,
			Error::Connection(_) => std::io::ErrorKind::NotConnected,
		};
		Self::new(kind, x)
	}
}

/// What the `web_transport_trait` impls report, which have one error type per session.
impl From<Error> for ConnectionError {
	fn from(err: Error) -> Self {
		match err {
			Error::Stopped(code) => Self::Stop(code.into_inner()),
			Error::Connection(err) => err,
			err => Self::Quic(err.to_string()),
		}
	}
}
