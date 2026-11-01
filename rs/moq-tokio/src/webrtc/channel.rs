//! qmux message I/O over one reliable, ordered data channel.
//!
//! Each SCTP message carries one qmux-01 record without its size field, as a
//! WebSocket message does. The peer's driver task owns the channel; these halves
//! only hand messages to and from it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Poll, ready};

use bytes::Bytes;
use tokio::sync::mpsc;

/// The data channel, as the qmux transport a [`qmux::Session`] runs over.
pub(crate) struct Channel {
	pub writer: Writer,
	pub reader: Reader,
}

impl qmux::transport::Transport for Channel {
	fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes, qmux::Error>> {
		self.reader.poll_recv(waiter)
	}

	fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), qmux::Error>> {
		self.writer.poll_ready(waiter)
	}

	fn send(&mut self, record: Bytes) -> Result<(), qmux::Error> {
		self.writer.send(record)
	}

	fn poll_flush(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), qmux::Error>> {
		self.writer.poll_ready(waiter)
	}

	fn poll_close(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), qmux::Error>> {
		ready!(self.writer.poll_ready(waiter))?;
		self.writer.close();
		Poll::Ready(Ok(()))
	}
}

/// A queue of outbound records, holding one at a time: the driver waits for SCTP to drain
/// before taking more, so a full send window holds back qmux's writer.
///
/// Closing it from either end ends the session's writes: the [`Writer`] closes it to have the
/// driver close the channel, and the driver drops its [`Outbox`] once the channel is gone.
pub(crate) fn outbox(max: usize) -> (Outbox, Writer) {
	let queue = kio::Queue::bounded(1);
	let writer = Writer {
		queue: queue.clone(),
		pending: None,
		max,
		_driver: None,
	};
	(Outbox(queue), writer)
}

/// The driver's end of the outbound queue.
pub(crate) struct Outbox(kio::Queue<Bytes>);

impl Outbox {
	/// The session's next record, or `None` once its writer closed.
	pub async fn next(&self) -> Option<Bytes> {
		self.0.pop().await.ok()
	}
}

impl Drop for Outbox {
	fn drop(&mut self) {
		self.0.close();
	}
}

/// Hands outbound records to the driver through its [`Outbox`].
pub(crate) struct Writer {
	queue: kio::Queue<Bytes>,
	/// A record waiting for room in `queue`.
	pending: Option<Bytes>,
	/// The largest message the peer accepts.
	max: usize,
	/// Keeps the driver running while the session outlives the signaling handle.
	pub _driver: Option<mpsc::Sender<super::Command>>,
}

impl Writer {
	/// Wait until the pending record, if any, is in the queue.
	pub fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), qmux::Error>> {
		if self.pending.is_some() {
			let pending = &mut self.pending;
			ready!(self.queue.poll_push_with(waiter, || pending.take().expect("checked above")))
				.map_err(|_| qmux::Error::Closed)?;
		}
		Poll::Ready(Ok(()))
	}

	/// Queue a record, after [`Self::poll_ready`] found room.
	pub fn send(&mut self, record: Bytes) -> Result<(), qmux::Error> {
		if record.len() > self.max {
			return Err(qmux::Error::FrameTooLarge);
		}
		self.pending = Some(record);
		Ok(())
	}

	/// Queue a record and wait for the driver to take it.
	#[cfg(test)]
	pub async fn write(&mut self, record: Bytes) -> Result<(), qmux::Error> {
		self.send(record)?;
		kio::wait(|waiter| self.poll_ready(waiter)).await
	}

	/// Close the channel once the driver takes what is queued.
	pub fn close(&mut self) {
		self.queue.close();
	}
}

impl Drop for Writer {
	fn drop(&mut self) {
		self.close();
	}
}

/// A queue of inbound records bounded by bytes the session has yet to read.
///
/// The driver never stops reading the socket to wait for the session: that would
/// also stall ICE consent checks and the SACKs that free our own send buffer.
/// qmux's flow control bounds what an honest peer can have in flight, so the
/// queue only overflows when the peer ignores it, and then the channel closes.
pub(crate) fn queue(budget: u64) -> (Inbox, Reader) {
	let (tx, rx) = mpsc::unbounded_channel();
	let unread = Arc::new(AtomicU64::new(0));
	let inbox = Inbox {
		tx,
		unread: unread.clone(),
		budget,
	};
	(inbox, Reader { rx, unread })
}

/// The driver's end of the inbound queue.
pub(crate) struct Inbox {
	tx: mpsc::UnboundedSender<Result<Bytes, qmux::Error>>,
	/// Bytes queued but not yet read, shared with the [`Reader`].
	unread: Arc<AtomicU64>,
	budget: u64,
}

impl Inbox {
	/// Queue a record for the session, returning false once its reads have ended:
	/// on an error, or with a flow control error when the record would leave more
	/// than the budget unread.
	pub fn push(&self, record: Result<Bytes, qmux::Error>) -> bool {
		// A closed reader drops the record.
		let data = match record {
			Ok(data) => data,
			Err(err) => {
				let _ = self.tx.send(Err(err));
				return false;
			}
		};
		// qmux records are never empty, so the byte budget also bounds their count.
		if data.is_empty() {
			let _ = self.tx.send(Err(qmux::Error::ProtocolViolation));
			return false;
		}
		let len = data.len() as u64;
		if self.unread.fetch_add(len, Ordering::Relaxed) + len > self.budget {
			let _ = self.tx.send(Err(qmux::Error::FlowControlError));
			return false;
		}
		let _ = self.tx.send(Ok(data));
		true
	}
}

/// Receives the records the driver read off the channel.
pub(crate) struct Reader {
	rx: mpsc::UnboundedReceiver<Result<Bytes, qmux::Error>>,
	unread: Arc<AtomicU64>,
}

impl Reader {
	/// The next record the driver read off the channel.
	pub fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes, qmux::Error>> {
		let record = ready!(self.rx.poll_recv(&mut waiter.context())).unwrap_or(Err(qmux::Error::Closed))?;
		self.unread.fetch_sub(record.len() as u64, Ordering::Relaxed);
		Poll::Ready(Ok(record))
	}

	/// Wait for [`Self::poll_recv`].
	#[cfg(test)]
	pub async fn recv(&mut self) -> Result<Bytes, qmux::Error> {
		kio::wait(|waiter| self.poll_recv(waiter)).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn queue_closes_past_its_budget() {
		let (inbox, mut reader) = queue(10);
		assert!(inbox.push(Ok(Bytes::from_static(&[0; 6]))));
		assert!(inbox.push(Ok(Bytes::from_static(&[1; 4]))));
		// Reading frees budget for the next record.
		assert_eq!(reader.recv().await.unwrap().len(), 6);
		assert!(inbox.push(Ok(Bytes::from_static(&[2; 6]))));
		assert!(!inbox.push(Ok(Bytes::from_static(&[3; 1]))));

		// Records queued before the overflow still arrive, then the error.
		assert_eq!(reader.recv().await.unwrap()[0], 1);
		assert_eq!(reader.recv().await.unwrap()[0], 2);
		assert!(matches!(reader.recv().await, Err(qmux::Error::FlowControlError)));
	}

	#[tokio::test]
	async fn queue_ends_on_empty_or_error_records() {
		// Neither costs any bytes, so the budget alone would never stop a flood of them.
		let (inbox, mut reader) = queue(10);
		assert!(!inbox.push(Ok(Bytes::new())));
		assert!(matches!(reader.recv().await, Err(qmux::Error::ProtocolViolation)));

		let (inbox, mut reader) = queue(10);
		assert!(!inbox.push(Err(qmux::Error::FrameTooLarge)));
		assert!(matches!(reader.recv().await, Err(qmux::Error::FrameTooLarge)));
	}
}
