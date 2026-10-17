//! qmux message I/O over one reliable, ordered data channel.
//!
//! Each SCTP message carries one qmux-01 record without its size field, as a
//! WebSocket message does. The peer's driver task owns the channel; these halves
//! only hand messages to and from it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use tokio::sync::mpsc;

/// The data channel, as the qmux transport a [`qmux::Session`] runs over.
pub(crate) struct Channel {
	pub writer: Writer,
	pub reader: Reader,
}

impl qmux::transport::Transport for Channel {
	type Writer = Writer;
	type Reader = Reader;

	fn split(self) -> (Writer, Reader) {
		(self.writer, self.reader)
	}
}

/// Hands outbound records to the driver, which waits for SCTP to drain before
/// taking more, so a full send window holds back qmux's writer.
pub(crate) struct Writer {
	/// `None` once closed: dropping the sender is what tells the driver to close the channel.
	pub tx: Option<mpsc::Sender<Bytes>>,
	/// The largest message the peer accepts.
	pub max: usize,
	/// Keeps the driver running while the session outlives the signaling handle.
	pub _driver: Option<mpsc::UnboundedSender<super::Command>>,
}

impl qmux::transport::Writer for Writer {
	async fn send(&mut self, data: Bytes) -> Result<(), qmux::Error> {
		if data.len() > self.max {
			return Err(qmux::Error::FrameTooLarge);
		}
		let tx = self.tx.as_ref().ok_or(qmux::Error::Closed)?;
		tx.send(data).await.map_err(|_| qmux::Error::Closed)
	}

	async fn close(&mut self) -> Result<(), qmux::Error> {
		self.tx = None;
		Ok(())
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

impl qmux::transport::Reader for Reader {
	async fn recv(&mut self) -> Result<Bytes, qmux::Error> {
		let record = self.rx.recv().await.unwrap_or(Err(qmux::Error::Closed))?;
		self.unread.fetch_sub(record.len() as u64, Ordering::Relaxed);
		Ok(record)
	}
}

#[cfg(test)]
mod tests {
	use qmux::transport::Reader as _;

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
