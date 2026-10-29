//! Stream handles: thin, direct calls into the shared moq-quic connection.
//!
//! Single-threaded sans-IO means a write goes straight into moq-quic's send
//! queue (no staging copy) and a read comes straight out of its reassembly
//! buffer; the handles just kick the driver so egress reaches the wire and
//! park on the per-stream waiter lists the driver wakes.

use std::task::{Context, Poll, ready};

use std::cell::RefMut;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use moq_quic::{LeaseError, StreamId, VarInt};

use super::super::Error;
use super::{End, Shared};

/// An outgoing stream. Dropping it unfinished resets it with code 0.
pub struct SendStream {
	shared: Shared,
	id: StreamId,
	park: kio::Park,
	/// The FIN went out; further writes are refused and `poll_closed` waits
	/// for the acknowledgement.
	fin: bool,
	/// We reset the stream; it is as closed as it will ever be.
	reset: bool,
}

impl SendStream {
	pub(crate) fn new(shared: Shared, id: StreamId) -> Self {
		// The driver records how this stream ends only while a handle holds
		// it, so the handle is what announces itself.
		shared.track(id);
		Self {
			shared,
			id,
			park: kio::Park::default(),
			fin: false,
			reset: false,
		}
	}

	/// The QUIC stream id, which the WebTransport layer uses as the session id.
	pub(crate) fn id(&self) -> u64 {
		self.id.into()
	}

	/// Whether the send side is already terminated, so [`Drop`] would do
	/// nothing. The WebTransport wrapper asks before mapping a reset of its
	/// own, since a finished stream must not be reset instead.
	pub(crate) fn ended(&self) -> bool {
		self.fin || self.reset
	}

	/// How many bytes were written so far, for a later [`reset_code`](Self::reset_code) to deliver.
	pub(crate) fn offset(&self) -> u64 {
		self.shared.conn.borrow_mut().send_stream(self.id).offset()
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	///
	/// Higher goes first.
	pub fn set_priority(&mut self, priority: i64) {
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.send_stream(self.id)
			.set_priority(priority);
	}

	/// Move the stream into a send group, or with `None` back to the
	/// ungrouped streams, which go before every group.
	///
	/// # Panics
	/// When `group` belongs to another connection.
	pub fn set_group(&mut self, group: Option<&super::SendGroup>) {
		let group = group.map(super::SendGroup::id);
		let _ = self.shared.conn.borrow_mut().send_stream(self.id).set_group(group);
	}

	/// [`reset`](web_transport_trait::poll::SendStream::reset) with a
	/// full-width code, for the WebTransport HTTP/3 error mapping, that still
	/// delivers the first `offset` bytes.
	pub(crate) fn reset_code(&mut self, code: u64, offset: Option<u64>) {
		if self.reset {
			return;
		}
		let code = VarInt::from_u64(code).unwrap_or(VarInt::MAX);
		let mut conn = self.shared.conn.borrow_mut();
		// Err means the stream is already gone, which is what we wanted.
		let _ = match offset {
			Some(offset) => conn.send_stream(self.id).reset_at(offset, code),
			None => conn.send_stream(self.id).reset(code),
		};
		drop(conn);
		self.reset = true;
		self.shared.kick();
	}
}

impl web_transport_trait::poll::SendStream for SendStream {
	type Error = Error;

	fn poll_write(&mut self, cx: &mut Context<'_>, mut buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		self.poll_write_buf(cx, &mut buf)
	}

	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.fin || self.reset {
			return Poll::Ready(Err(Error::Quic("stream already finished".to_string())));
		}
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		let result = {
			let mut conn = self.shared.conn.borrow_mut();
			let mut stream = conn.send_stream(self.id);
			// Bytes taken from `buf` cannot go back, so take only what moq-quic
			// accepts. It keeps the chunk by reference, which for a `Bytes`
			// source means no copy at all.
			match stream.write_limit().min(buf.chunk().len()) {
				// Blocked, failing, or nothing to write; the empty write says which.
				0 => stream.write(&[]),
				take => stream.write_chunks(&mut [buf.copy_to_bytes(take)]).map(|written| {
					debug_assert_eq!(written.bytes, take, "moq-quic accepted less than its write limit");
					written.bytes
				}),
			}
		};
		match result {
			Ok(n) => {
				self.shared.kick();
				Poll::Ready(Ok(n))
			}
			// No capacity right now; the driver wakes us when moq-quic reports
			// the stream writable.
			Err(moq_quic::WriteError::Blocked) => {
				self.shared.park_writable(self.id, waiter);
				Poll::Pending
			}
			Err(moq_quic::WriteError::Stopped(code)) => Poll::Ready(Err(Error::Stop(code.into_inner()))),
			Err(moq_quic::WriteError::ClosedStream) => {
				Poll::Ready(Err(Error::Quic("stream already finished".to_string())))
			}
		}
	}

	fn set_priority(&mut self, order: i32) {
		// The trait (like W3C sendOrder) sends HIGHER values first, and so
		// does moq-quic.
		SendStream::set_priority(self, order.into());
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		if self.fin || self.reset {
			return Ok(());
		}
		match self.shared.conn.borrow_mut().send_stream(self.id).finish() {
			Ok(()) => {}
			// A STOP_SENDING beat us here. Carry the code like `poll_write`
			// does, or `moq_net::Error::from_transport` cannot decode a
			// routine cancellation.
			Err(moq_quic::FinishError::Stopped(code)) => {
				self.reset = true;
				return Err(Error::Stop(code.into_inner()));
			}
			// Already finished or reset, so the FIN it wanted is out.
			Err(moq_quic::FinishError::ClosedStream) => {}
		}
		self.fin = true;
		self.shared.kick();
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		self.reset_code(u64::from(code), None);
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.reset {
			return Poll::Ready(Ok(()));
		}
		// moq-quic reports a send stream's end as an event, which the driver
		// records: an acknowledged FIN, or the peer's STOP_SENDING.
		match self.shared.ended(self.id) {
			Some(End::Stopped(code)) => Poll::Ready(Err(Error::Stop(code))),
			Some(End::Delivered) => Poll::Ready(Ok(())),
			// The end never came. If the connection died first the caller
			// cannot read success as "every byte arrived".
			None => match self.shared.closed() {
				Some(err) => Poll::Ready(Err(err)),
				None => {
					self.shared.park_finishing(self.id, waiter);
					Poll::Pending
				}
			},
		}
	}
}

impl Drop for SendStream {
	fn drop(&mut self) {
		self.shared.forget_send(self.id);
		if !self.fin && !self.reset {
			let _ = self
				.shared
				.conn
				.borrow_mut()
				.send_stream(self.id)
				.reset(VarInt::from_u32(0));
			self.shared.kick();
		}
	}
}

impl std::fmt::Debug for SendStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("SendStream").field("id", &self.id).finish()
	}
}

/// One read out of moq-quic's reassembly buffer.
enum Read {
	/// Bytes, at most as many as were asked for.
	Chunk(Bytes),
	/// Every byte up to the FIN has been handed over.
	Finished,
	/// Nothing buffered; the driver wakes us when that changes.
	Blocked,
	/// The peer reset the stream with this code.
	Reset(u64),
}

/// An incoming stream. Dropping it unfinished sends STOP_SENDING with code 0.
pub struct RecvStream {
	shared: Shared,
	id: StreamId,
	park: kio::Park,
	/// Every byte up to the FIN was read out of moq-quic.
	finished: bool,
	/// We stopped the stream; no more reads matter.
	stopped: bool,
}

impl RecvStream {
	pub(crate) fn new(shared: Shared, id: StreamId) -> Self {
		Self {
			shared,
			id,
			park: kio::Park::default(),
			finished: false,
			stopped: false,
		}
	}

	/// Lease the stream's unreleased bytes once at least `want` have arrived
	/// in order, or the stream ended.
	///
	/// The bytes stay in moq-quic's receive buffer, keeping their connection
	/// flow control credit, until read through the lease as a [`Buf`] and
	/// then [`Lease::release`]d; bytes not read stay for the next lease.
	/// Stream credit still advances, so a stream window smaller than a
	/// message keeps the message flowing.
	///
	/// When the peer has spent the connection window and no reader can make
	/// progress, the stream ranked lowest by [`Self::set_priority`] is
	/// evicted: its buffered bytes and their credit are discarded and its next
	/// lease fails with [`Error::Evicted`]. Bytes it took keep their credit
	/// until the stream is stopped. Nothing is evicted while the leased and
	/// unread bytes of every stream stay below the connection receive window.
	///
	/// Do not mix with the plain reads on one stream, which skip the leased
	/// bytes.
	pub fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<Lease<'_>, Error>> {
		let waiter = self.park.hold(cx);
		let mut conn = self.shared.conn.borrow_mut();
		let err = match conn.recv_stream(self.id).lease(want) {
			Ok(transmit) => {
				if transmit.should_transmit() {
					self.shared.kick();
				}
				return Poll::Ready(Ok(Lease {
					conn,
					shared: &self.shared,
					id: self.id,
				}));
			}
			Err(LeaseError::Blocked(transmit)) => {
				drop(conn);
				if transmit.should_transmit() {
					self.shared.kick();
				}
				if let Some(err) = self.shared.closed() {
					return Poll::Ready(Err(err));
				}
				self.shared.park_readable(self.id, waiter);
				return Poll::Pending;
			}
			Err(LeaseError::Reset(code)) => Error::Reset(code.into_inner()),
			Err(LeaseError::Evicted) => Error::Evicted,
			Err(LeaseError::TooLarge) => Error::TooLarge,
			Err(LeaseError::ClosedStream) => {
				drop(conn);
				self.shared.closed().unwrap_or(Error::Closed)
			}
		};
		Poll::Ready(Err(err))
	}

	/// Rank this stream for eviction: when the connection window is stuck,
	/// the stream with the lowest priority goes first, and the newest of
	/// those that tie.
	pub fn set_priority(&mut self, priority: i64) {
		self.shared
			.conn
			.borrow_mut()
			.recv_stream(self.id)
			.set_priority(priority);
	}

	/// [`poll_read`](web_transport_trait::poll::RecvStream::poll_read).
	fn poll_read_into(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Error>> {
		let Some(chunk) = ready!(self.poll_chunk(cx, dst.len()))? else {
			return Poll::Ready(Ok(None));
		};
		dst[..chunk.len()].copy_from_slice(&chunk);
		Poll::Ready(Ok(Some(chunk.len())))
	}

	/// [`poll_read_chunk`](web_transport_trait::poll::RecvStream::poll_read_chunk).
	fn poll_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Error>> {
		let waiter = self.park.hold(cx);
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		match Self::read(&self.shared, self.id, max) {
			// moq-quic hands out at most `max`.
			Read::Chunk(bytes) => Poll::Ready(Ok(Some(bytes))),
			Read::Finished => {
				self.finished = true;
				Poll::Ready(Ok(None))
			}
			Read::Blocked => {
				if let Some(err) = self.shared.closed() {
					return Poll::Ready(Err(err));
				}
				self.shared.park_readable(self.id, waiter);
				Poll::Pending
			}
			Read::Reset(code) => Poll::Ready(Err(Error::Reset(code))),
		}
	}

	/// Whether the read side is already terminated, so [`Drop`] would do
	/// nothing.
	pub(crate) fn ended(&self) -> bool {
		self.finished || self.stopped
	}

	/// [`stop`](web_transport_trait::poll::RecvStream::stop) with a
	/// full-width code, for the WebTransport HTTP/3 error mapping.
	pub(crate) fn stop_code(&mut self, code: u64) {
		if self.stopped || self.finished {
			return;
		}
		// Err means the stream is already gone, which is what we wanted.
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.recv_stream(self.id)
			.stop(VarInt::from_u64(code).unwrap_or(VarInt::MAX));
		self.stopped = true;
		self.shared.kick();
	}

	/// Take up to `max` bytes out of moq-quic's reassembly buffer.
	///
	/// Reading is what returns the peer's flow control credit, so a read that
	/// owes it a frame kicks the driver. A `max` of 0 takes nothing, and still
	/// reports the FIN or a reset once nothing is left before it.
	///
	/// Takes the shared state rather than `&mut self` so a caller can hold a
	/// waiter from `self.park` across the read.
	fn read(shared: &Shared, id: StreamId, max: usize) -> Read {
		let mut conn = shared.conn.borrow_mut();
		let mut recv = conn.recv_stream(id);
		let mut chunks = match recv.read() {
			Ok(chunks) => chunks,
			// The stream is gone, so everything it held is already ours.
			Err(_) => return Read::Finished,
		};
		let read = match chunks.next(max) {
			Ok(Some(chunk)) => Read::Chunk(chunk.bytes),
			Ok(None) => Read::Finished,
			Err(moq_quic::ReadError::Blocked) => Read::Blocked,
			Err(moq_quic::ReadError::Reset(code)) => Read::Reset(code.into_inner()),
		};
		let transmit = chunks.finalize().should_transmit();
		drop(conn);
		if transmit {
			shared.kick();
		}
		read
	}
}

impl web_transport_trait::poll::RecvStream for RecvStream {
	type Error = Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		self.poll_read_into(cx, dst)
	}

	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		self.poll_chunk(cx, max)
	}

	fn stop(&mut self, code: u32) {
		self.stop_code(u64::from(code));
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.finished || self.stopped {
			return Poll::Ready(Ok(()));
		}
		// An empty read observes the end without taking the bytes before it, so
		// the stream closes once those are read.
		match Self::read(&self.shared, self.id, 0) {
			Read::Finished => {
				self.finished = true;
				Poll::Ready(Ok(()))
			}
			Read::Reset(code) => Poll::Ready(Err(Error::Reset(code))),
			Read::Chunk(_) | Read::Blocked => {
				if let Some(err) = self.shared.closed() {
					return Poll::Ready(Err(err));
				}
				self.shared.park_readable(self.id, waiter);
				Poll::Pending
			}
		}
	}
}

impl Drop for RecvStream {
	fn drop(&mut self) {
		self.shared.forget_recv(self.id);
		if !self.finished && !self.stopped {
			let _ = self
				.shared
				.conn
				.borrow_mut()
				.recv_stream(self.id)
				.stop(VarInt::from_u32(0));
			self.shared.kick();
		}
	}
}

impl std::fmt::Debug for RecvStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RecvStream").field("id", &self.id).finish()
	}
}

/// Unreleased bytes borrowed from a [`RecvStream`] by [`RecvStream::poll_lease`],
/// read as a [`Buf`].
///
/// Borrows the connection for as long as it lives.
pub struct Lease<'a> {
	conn: RefMut<'a, moq_quic::connection::Connection>,
	shared: &'a Shared,
	id: StreamId,
}

impl Lease<'_> {
	fn leased(&self) -> &moq_quic::Leased {
		self.conn.leased(self.id).expect("a lease outlives nothing it borrows")
	}

	/// Whether the peer finished the stream right after the leased bytes.
	pub fn is_fin(&self) -> bool {
		self.leased().is_fin()
	}

	/// The bytes not yet read, in order, one contiguous chunk at a time.
	pub fn chunks(&self) -> impl Iterator<Item = &[u8]> {
		self.leased().chunks()
	}

	/// Return the credit of every byte read so far, by this lease or an
	/// earlier one.
	pub fn release(self) {
		let Self { mut conn, shared, id } = self;
		let transmit = conn.recv_stream(id).release().should_transmit();
		drop(conn);
		if transmit {
			shared.kick();
		}
	}
}

/// Reading moves past bytes for good, but they keep their credit until
/// [`Lease::release`].
impl Buf for Lease<'_> {
	fn remaining(&self) -> usize {
		self.leased().len()
	}

	fn chunk(&self) -> &[u8] {
		self.leased().chunks().next().unwrap_or_default()
	}

	fn advance(&mut self, n: usize) {
		self.conn.recv_stream(self.id).advance(n);
	}

	fn copy_to_bytes(&mut self, n: usize) -> Bytes {
		match n <= self.chunk().len() {
			true => self.conn.recv_stream(self.id).take(n),
			false => {
				let mut bytes = BytesMut::with_capacity(n);
				bytes.put((&mut *self).take(n));
				bytes.freeze()
			}
		}
	}
}
