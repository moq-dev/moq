//! qmux message I/O over one reliable, ordered data channel.
//!
//! Each SCTP message carries one qmux-01 record without its size field, as a
//! WebSocket message does. The peer's driver task owns the channel; these halves
//! only hand messages to and from it.

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

/// Receives the records the driver read off the channel.
pub(crate) struct Reader {
	/// Bounded: while it is full, the driver stops reading the socket.
	pub rx: mpsc::Receiver<Result<Bytes, qmux::Error>>,
}

impl qmux::transport::Reader for Reader {
	async fn recv(&mut self) -> Result<Bytes, qmux::Error> {
		self.rx.recv().await.unwrap_or(Err(qmux::Error::Closed))
	}
}
