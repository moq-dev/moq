//! Stream handles: thin, direct calls into the shared moq-quic connection.
//!
//! Single-threaded sans-IO means a write goes straight into moq-quic's send
//! queue (no staging copy) and a read comes straight out of its reassembly
//! buffer; the handles just kick the driver so egress reaches the wire and
//! park on the per-stream waiter lists the driver wakes.

use std::task::{Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use moq_quic::{Checkpoint, StreamId, VarInt};

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

	/// Mark the data written so far, for a later [`reset_code`](Self::reset_code) to deliver.
	pub(crate) fn checkpoint(&self) -> Checkpoint {
		self.shared.conn.borrow_mut().send_stream(self.id).checkpoint()
	}

	/// [`reset`](web_transport_trait::poll::SendStream::reset) with a
	/// full-width code, for the WebTransport HTTP/3 error mapping, that still
	/// delivers the data before `checkpoint`, one this stream took.
	pub(crate) fn reset_code(&mut self, code: u64, checkpoint: Option<Checkpoint>) {
		if self.reset {
			return;
		}
		let code = VarInt::from_u64(code).unwrap_or(VarInt::MAX);
		let mut conn = self.shared.conn.borrow_mut();
		// Err means the stream is already gone, which is what we wanted.
		let _ = match checkpoint {
			Some(checkpoint) => checkpoint.reset(&mut conn, code),
			None => conn.send_stream(self.id).reset(code),
		};
		drop(conn);
		self.reset = true;
		self.shared.kick();
	}
}

impl web_transport_trait::poll::SendStream for SendStream {
	type Error = Error;

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.fin || self.reset {
			return Poll::Ready(Err(Error::Quic("stream already finished".to_string())));
		}
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		let result = self.shared.conn.borrow_mut().send_stream(self.id).write(buf);
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
		let _ = self.shared.conn.borrow_mut().send_stream(self.id).set_priority(order);
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

/// How far [`RecvStream::poll_closed`] reads ahead of the application before
/// it waits for the backlog to drain.
const READ_AHEAD: usize = 64 * 1024;
/// How much to ask for per read-ahead chunk.
const READ_CHUNK: usize = 8 * 1024;

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
	/// Every byte up to the FIN was read out of moq-quic; reads report the end
	/// once `backlog` is drained too.
	finished: bool,
	/// We stopped the stream; no more reads matter.
	stopped: bool,
	/// Bytes `poll_closed` read ahead, handed to `poll_read` before moq-quic's.
	backlog: BytesMut,
	/// The connection credit for `backlog`, kept until the application reads
	/// it, as if it were still in moq-quic's reassembly buffer.
	backlog_credit: moq_quic::Credit,
}

impl RecvStream {
	pub(crate) fn new(shared: Shared, id: StreamId) -> Self {
		Self {
			shared,
			id,
			park: kio::Park::default(),
			finished: false,
			stopped: false,
			backlog: BytesMut::new(),
			backlog_credit: moq_quic::Credit::default(),
		}
	}

	/// Read into `dst`, keeping the connection flow control credit for exactly
	/// the bytes read, or `None` once the peer has finished the stream.
	///
	/// Stream credit still advances, so a stream window smaller than an
	/// application message keeps the message flowing, while the connection
	/// window these bytes occupy stays spent until the credit is dropped.
	pub fn poll_read_held(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<Credit>, Error>> {
		let mut held = moq_quic::Credit::default();
		self.poll_read_into(cx, dst, Some(&mut held)).map_ok(|read| {
			read.map(|_| Credit {
				shared: self.shared.clone(),
				inner: held,
			})
		})
	}

	/// [`poll_read`](web_transport_trait::poll::RecvStream::poll_read), with
	/// the credit for the bytes read moved into `held` instead of returned to
	/// the peer.
	fn poll_read_into(
		&mut self,
		cx: &mut Context<'_>,
		dst: &mut [u8],
		held: Option<&mut moq_quic::Credit>,
	) -> Poll<Result<Option<usize>, Error>> {
		let waiter = self.park.hold(cx);
		if dst.is_empty() {
			return Poll::Ready(Ok(Some(0)));
		}
		if !self.backlog.is_empty() {
			return Poll::Ready(Ok(Some(self.drain(dst, held))));
		}
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		match Self::read(&self.shared, self.id, dst.len(), held) {
			Read::Chunk(bytes) => {
				// moq-quic hands out at most `dst.len()`.
				dst[..bytes.len()].copy_from_slice(&bytes);
				Poll::Ready(Ok(Some(bytes.len())))
			}
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
		release(&self.shared, std::mem::take(&mut self.backlog_credit));
		self.backlog.clear();
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
	/// owes it a frame kicks the driver. The connection credit for the bytes
	/// read moves into `held` instead, when given.
	///
	/// Takes the shared state rather than `&mut self` so a caller can hold a
	/// waiter from `self.park` across the read.
	fn read(shared: &Shared, id: StreamId, max: usize, held: Option<&mut moq_quic::Credit>) -> Read {
		let mut conn = shared.conn.borrow_mut();
		let mut recv = conn.recv_stream(id);
		let mut chunks = match recv.read(true) {
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
		if let Some(held) = held {
			held.merge(chunks.hold());
		}
		let transmit = chunks.finalize().should_transmit();
		drop(conn);
		if transmit {
			shared.kick();
		}
		read
	}

	/// Move up to `dst.len()` read-ahead bytes out of the backlog, with their
	/// credit moved into `held` or else returned to the peer.
	///
	/// Wakes a `poll_closed` parked at the read-ahead cap: the room it was
	/// waiting for is what this just made.
	fn drain(&mut self, dst: &mut [u8], held: Option<&mut moq_quic::Credit>) -> usize {
		let n = dst.len().min(self.backlog.len());
		dst[..n].copy_from_slice(&self.backlog[..n]);
		self.backlog.advance(n);
		let credit = self.backlog_credit.split(n as u64);
		match held {
			Some(held) => held.merge(credit),
			None => release(&self.shared, credit),
		}
		if n > 0 {
			self.shared.wake_readable(self.id);
		}
		n
	}
}

impl web_transport_trait::poll::RecvStream for RecvStream {
	type Error = Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		self.poll_read_into(cx, dst, None)
	}

	fn stop(&mut self, code: u32) {
		// Giving up on the read side abandons whatever was read ahead, even
		// when the FIN is already in and only the backlog is left.
		self.stop_code(u64::from(code));
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.finished || self.stopped {
			return Poll::Ready(Ok(()));
		}
		// The FIN sits behind whatever the peer sent before it, and moq-quic only
		// reports the stream finished once that is read out. Waiting on
		// readability alone would park behind bytes nobody is reading, so read
		// ahead into the backlog `poll_read` serves first: this watch resolves
		// without the application draining the stream, and without losing what
		// it might still want.
		loop {
			if self.backlog.len() >= READ_AHEAD {
				// Enough held: reading further would let the peer send more
				// still, so the memory bound wins over watch liveness here.
				// `drain` wakes this once the application takes some, and a
				// stream nobody reads past the cap keeps its watch pending
				// until the connection's idle timeout.
				self.shared.park_readable(self.id, waiter);
				return Poll::Pending;
			}
			match Self::read(&self.shared, self.id, READ_CHUNK, Some(&mut self.backlog_credit)) {
				Read::Chunk(bytes) => self.backlog.extend_from_slice(&bytes),
				Read::Finished => {
					self.finished = true;
					return Poll::Ready(Ok(()));
				}
				Read::Blocked => {
					if let Some(err) = self.shared.closed() {
						return Poll::Ready(Err(err));
					}
					self.shared.park_readable(self.id, waiter);
					return Poll::Pending;
				}
				Read::Reset(code) => return Poll::Ready(Err(Error::Reset(code))),
			}
		}
	}
}

impl Drop for RecvStream {
	fn drop(&mut self) {
		release(&self.shared, std::mem::take(&mut self.backlog_credit));
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

/// Return `credit` to the peer, kicking the driver if the peer should hear.
fn release(shared: &Shared, credit: moq_quic::Credit) {
	if credit.is_empty() {
		return;
	}
	if shared.conn.borrow_mut().release(credit).should_transmit() {
		shared.kick();
	}
}

/// Connection flow control credit for bytes read with
/// [`RecvStream::poll_read_held`].
///
/// The peer cannot reuse the connection window these bytes occupy until this
/// is dropped, which returns it even after the stream or connection is gone.
pub struct Credit {
	shared: Shared,
	inner: moq_quic::Credit,
}

impl Credit {
	/// The number of bytes this credit covers.
	pub fn len(&self) -> u64 {
		self.inner.len()
	}

	/// Whether this credit covers no bytes.
	pub fn is_empty(&self) -> bool {
		self.inner.is_empty()
	}

	/// Split off the credit for the first `bytes`, keeping the rest.
	///
	/// # Panics
	/// If `bytes` exceeds [`Self::len()`].
	pub fn split(&mut self, bytes: u64) -> Self {
		Self {
			shared: self.shared.clone(),
			inner: self.inner.split(bytes),
		}
	}

	/// Take over the bytes covered by `other`.
	///
	/// # Panics
	/// If `other` came from another connection.
	pub fn merge(&mut self, mut other: Self) {
		assert!(
			std::rc::Rc::ptr_eq(&self.shared, &other.shared),
			"merged credit from another connection"
		);
		self.inner.merge(std::mem::take(&mut other.inner));
	}
}

impl Drop for Credit {
	fn drop(&mut self) {
		release(&self.shared, std::mem::take(&mut self.inner));
	}
}

impl std::fmt::Debug for Credit {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Credit").field("len", &self.len()).finish()
	}
}
