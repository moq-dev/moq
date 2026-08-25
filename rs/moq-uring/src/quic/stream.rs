//! Stream handles: thin, direct calls into the shared quiche connection.
//!
//! Single-threaded sans-IO means a write goes straight into quiche's send
//! queue (no staging copy) and a read comes straight out of its reassembly
//! buffer; the handles just kick the driver so egress reaches the wire and
//! park on the per-stream waiter lists the driver wakes.

use std::task::{Context, Poll};

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
		self.shared.conn.borrow_mut().stream_send(self.id, &[], true)?;
		self.fin = true;
		self.shared.kick();
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		if self.reset {
			return;
		}
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.stream_shutdown(self.id, quiche::Shutdown::Write, u64::from(code));
		self.reset = true;
		self.shared.kick();
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
			Err(_) => Poll::Ready(Ok(())),
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

/// An incoming stream. Dropping it unfinished sends STOP_SENDING with code 0.
pub struct RecvStream {
	shared: Shared,
	id: u64,
	park: kio::Park,
	/// Every byte up to the FIN was read; reads report the end from now on.
	finished: bool,
	/// We stopped the stream; no more reads matter.
	stopped: bool,
}

impl RecvStream {
	pub(crate) fn new(shared: Shared, id: u64) -> Self {
		Self {
			shared,
			id,
			park: kio::Park::default(),
			finished: false,
			stopped: false,
		}
	}
}

impl web_transport_trait::poll::RecvStream for RecvStream {
	type Error = Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.finished {
			return Poll::Ready(Ok(None));
		}
		if dst.is_empty() {
			return Poll::Ready(Ok(Some(0)));
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
		if self.stopped || self.finished {
			return;
		}
		let _ = self
			.shared
			.conn
			.borrow_mut()
			.stream_shutdown(self.id, quiche::Shutdown::Read, u64::from(code));
		self.stopped = true;
		self.shared.kick();
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let waiter = self.park.hold(cx);
		if self.finished || self.stopped {
			return Poll::Ready(Ok(()));
		}
		// `stream_finished` also reports a reset stream: either way no more
		// data is coming, which is what "closed" means for the read side.
		if self.shared.conn.borrow().stream_finished(self.id) {
			return Poll::Ready(Ok(()));
		}
		if let Some(err) = self.shared.closed() {
			return Poll::Ready(Err(err));
		}
		self.shared.park_readable(self.id, waiter);
		Poll::Pending
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
