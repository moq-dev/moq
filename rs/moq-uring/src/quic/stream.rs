//! Stream handles: thin, direct calls into the shared quiche connection.
//!
//! Single-threaded sans-IO means a write goes straight into quiche's send
//! queue (no staging copy) and a read comes straight out of its reassembly
//! buffer; the handles just kick the driver so egress reaches the wire and
//! park on the per-stream waiter lists the driver wakes.

use std::task::{Context, Poll};

use bytes::{Buf, BytesMut};

use super::{Error, Shared};

/// An outgoing stream. Dropping it unfinished resets it with code 0.
pub struct SendStream {
	shared: Shared,
	id: u64,
	park: kio::Park,
	/// The FIN went out; further writes are refused and `poll_closed` waits
	/// for the acknowledgement.
	fin: bool,
	/// We reset the stream; it is as closed as it will ever be.
	reset: bool,
}

impl SendStream {
	pub(crate) fn new(shared: Shared, id: u64) -> Self {
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
		self.id
	}

	/// Queue as much of `buf` as quiche will take right now, without parking.
	/// Best-effort, for the close path where nobody is left to poll.
	pub(crate) fn try_write(&mut self, buf: &[u8]) -> usize {
		if self.fin || self.reset {
			return 0;
		}
		let n = self
			.shared
			.conn
			.borrow_mut()
			.stream_send(self.id, buf, false)
			.unwrap_or(0);
		if n > 0 {
			self.shared.kick();
		}
		n
	}

	/// [`reset`](web_transport_trait::poll::SendStream::reset) with a
	/// full-width code, for the WebTransport HTTP/3 error mapping.
	pub(crate) fn reset_code(&mut self, code: u64) {
		if self.reset {
			return;
		}
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.stream_shutdown(self.id, quiche::Shutdown::Write, code);
		self.reset = true;
		self.shared.kick();
	}
}

impl web_transport_trait::poll::SendStream for SendStream {
	type Error = Error;

	fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.fin || self.reset {
			return Poll::Ready(Err(Error::Quic(quiche::Error::FinalSize)));
		}
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		let result = self.shared.conn.borrow_mut().stream_send(self.id, buf, false);
		match result {
			Ok(n) if n > 0 || buf.is_empty() => {
				self.shared.kick();
				Poll::Ready(Ok(n))
			}
			// No capacity right now; the driver wakes us when quiche reports
			// the stream writable.
			Ok(_) | Err(quiche::Error::Done) => {
				self.shared.park_writable(self.id, waiter);
				Poll::Pending
			}
			Err(quiche::Error::StreamStopped(code)) => Poll::Ready(Err(Error::Stop(code))),
			Err(err) => Poll::Ready(Err(err.into())),
		}
	}

	fn set_priority(&mut self, order: u8) {
		// The trait (like W3C sendOrder) sends HIGHER values first; quiche
		// urgency is the opposite.
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.stream_priority(self.id, 255 - order, false);
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		if self.fin || self.reset {
			return Ok(());
		}
		// An empty FIN write succeeds even at zero capacity.
		match self.shared.conn.borrow_mut().stream_send(self.id, &[], true) {
			Ok(_) => {}
			// A STOP_SENDING beat us here. Carry the code like `poll_write`
			// does, or `moq_net::Error::from_transport` cannot decode a
			// routine cancellation.
			Err(quiche::Error::StreamStopped(code)) => {
				self.reset = true;
				return Err(Error::Stop(code));
			}
			Err(err) => return Err(err.into()),
		}
		self.fin = true;
		self.shared.kick();
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		self.reset_code(u64::from(code));
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.reset {
			return Poll::Ready(Ok(()));
		}
		// quiche collects a stream when its send side ends (FIN fully
		// acknowledged, a STOP_SENDING, or a reset), and a collected stream
		// refuses the capacity query. The driver probes this each turn.
		let result = self.shared.conn.borrow_mut().stream_capacity(self.id);
		match result {
			Err(quiche::Error::StreamStopped(code)) => Poll::Ready(Err(Error::Stop(code))),
			// The stream is gone. If the driver watched it end while the
			// connection was up, that verdict stands and a close afterwards
			// changes nothing. Otherwise we never found out, and the caller
			// reads success as "every byte arrived".
			Err(_) => match self.shared.collected(self.id) {
				Some(Some(code)) => Poll::Ready(Err(Error::Stop(code))),
				Some(None) => Poll::Ready(Ok(())),
				None => match self.shared.closed() {
					Some(err) => Poll::Ready(Err(err)),
					None => Poll::Ready(Ok(())),
				},
			},
			Ok(_) => {
				if let Some(err) = self.shared.closed() {
					return Poll::Ready(Err(err));
				}
				self.shared.park_finishing(self.id, waiter);
				Poll::Pending
			}
		}
	}
}

impl Drop for SendStream {
	fn drop(&mut self) {
		self.shared.forget(self.id);
		if !self.fin && !self.reset {
			let _ = self
				.shared
				.conn
				.borrow_mut()
				.stream_shutdown(self.id, quiche::Shutdown::Write, 0);
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

/// An incoming stream. Dropping it unfinished sends STOP_SENDING with code 0.
pub struct RecvStream {
	shared: Shared,
	id: u64,
	park: kio::Park,
	/// Every byte up to the FIN was read out of quiche; reads report the end
	/// once `backlog` is drained too.
	finished: bool,
	/// We stopped the stream; no more reads matter.
	stopped: bool,
	/// Bytes `poll_closed` read ahead, handed to `poll_read` before quiche's.
	backlog: BytesMut,
}

impl RecvStream {
	pub(crate) fn new(shared: Shared, id: u64) -> Self {
		Self {
			shared,
			id,
			park: kio::Park::default(),
			finished: false,
			stopped: false,
			backlog: BytesMut::new(),
		}
	}

	/// [`stop`](web_transport_trait::poll::RecvStream::stop) with a
	/// full-width code, for the WebTransport HTTP/3 error mapping.
	pub(crate) fn stop_code(&mut self, code: u64) {
		self.backlog.clear();
		if self.stopped || self.finished {
			return;
		}
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.stream_shutdown(self.id, quiche::Shutdown::Read, code);
		self.stopped = true;
		self.shared.kick();
	}

	/// Move up to `dst.len()` read-ahead bytes out of the backlog.
	///
	/// Wakes a `poll_closed` parked at the read-ahead cap: the room it was
	/// waiting for is what this just made.
	fn drain(&mut self, dst: &mut [u8]) -> usize {
		let n = dst.len().min(self.backlog.len());
		dst[..n].copy_from_slice(&self.backlog[..n]);
		self.backlog.advance(n);
		if n > 0 {
			self.shared.wake_readable(self.id);
		}
		n
	}
}

impl web_transport_trait::poll::RecvStream for RecvStream {
	type Error = Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		let waiter = self.park.hold(cx);
		if dst.is_empty() {
			return Poll::Ready(Ok(Some(0)));
		}
		if !self.backlog.is_empty() {
			return Poll::Ready(Ok(Some(self.drain(dst))));
		}
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		let mut conn = self.shared.conn.borrow_mut();
		match conn.stream_recv(self.id, dst) {
			Ok((n, fin)) => {
				if fin {
					self.finished = true;
					if n == 0 {
						return Poll::Ready(Ok(None));
					}
				}
				drop(conn);
				// Reading frees flow control the peer is waiting on.
				self.shared.kick();
				Poll::Ready(Ok(Some(n)))
			}
			Err(quiche::Error::Done) => {
				if conn.stream_finished(self.id) {
					self.finished = true;
					return Poll::Ready(Ok(None));
				}
				drop(conn);
				if let Some(err) = self.shared.closed() {
					return Poll::Ready(Err(err));
				}
				self.shared.park_readable(self.id, waiter);
				Poll::Pending
			}
			Err(quiche::Error::StreamReset(code)) => Poll::Ready(Err(Error::Reset(code))),
			Err(err) => Poll::Ready(Err(err.into())),
		}
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
		// The FIN sits behind whatever the peer sent before it, and quiche only
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
			let mut chunk = [0u8; 8 * 1024];
			let mut conn = self.shared.conn.borrow_mut();
			match conn.stream_recv(self.id, &mut chunk) {
				Ok((n, fin)) => {
					drop(conn);
					self.backlog.extend_from_slice(&chunk[..n]);
					// Reading frees flow control the peer is waiting on.
					self.shared.kick();
					if fin {
						self.finished = true;
						return Poll::Ready(Ok(()));
					}
				}
				Err(quiche::Error::Done) => {
					// Nothing buffered: either the FIN is in (a reset comes
					// back as `StreamReset` below, not as this) or more is
					// still coming.
					let finished = conn.stream_finished(self.id);
					drop(conn);
					if finished {
						self.finished = true;
						return Poll::Ready(Ok(()));
					}
					if let Some(err) = self.shared.closed() {
						return Poll::Ready(Err(err));
					}
					self.shared.park_readable(self.id, waiter);
					return Poll::Pending;
				}
				Err(quiche::Error::StreamReset(code)) => return Poll::Ready(Err(Error::Reset(code))),
				Err(err) => return Poll::Ready(Err(err.into())),
			}
		}
	}
}

impl Drop for RecvStream {
	fn drop(&mut self) {
		if !self.finished && !self.stopped {
			let _ = self
				.shared
				.conn
				.borrow_mut()
				.stream_shutdown(self.id, quiche::Shutdown::Read, 0);
			self.shared.kick();
		}
	}
}

impl std::fmt::Debug for RecvStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("RecvStream").field("id", &self.id).finish()
	}
}
