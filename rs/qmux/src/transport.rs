use std::task::Poll;

use bytes::Bytes;

use crate::{Config, Error};

/// Record I/O over a reliable transport, polled by a session's [`Driver`](crate::Driver).
///
/// Each record is one complete QMux record, or one frame on draft-00: a WebSocket message
/// carries one, and a byte stream delimits them itself. Use [`Stream`] for an
/// [`AsyncRead`](tokio::io::AsyncRead) + [`AsyncWrite`](tokio::io::AsyncWrite) byte stream,
/// and implement this trait for a transport with its own message boundaries or lifecycle.
///
/// The driver polls receiving and sending from one task, so a write blocked on the
/// transport never stalls reads, and the session sheds unreliable datagrams rather than
/// buffering them behind a stalled transport.
pub trait Transport {
	/// Bind the session's framing, before the first poll.
	fn bind(&mut self, _config: &Config) {}

	/// Receive the next complete record.
	///
	/// An error ends the receive side; the driver does not poll it again.
	fn poll_recv(&mut self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>>;

	/// Wait for room to queue another record with [`send`](Self::send).
	fn poll_ready(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>>;

	/// Queue one record, after [`poll_ready`](Self::poll_ready) found room.
	fn send(&mut self, record: Bytes) -> Result<(), Error>;

	/// Write out everything queued.
	///
	/// The driver polls this after every batch, and while idle, so a transport can piggyback
	/// its own control messages, such as a WebSocket Pong.
	fn poll_flush(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>>;

	/// Flush and gracefully close the transport. A byte stream also drains the peer until it
	/// finishes, which only the peer bounds, so the driver bounds the wait.
	fn poll_close(&mut self, waiter: &kio::Waiter) -> Poll<Result<(), Error>>;

	/// Statistics for the connection underneath, if it exposes any.
	///
	/// A transport that returns `None` leaves the session reporting only what it can measure
	/// itself, i.e. the `QX_PING` round trip.
	fn socket_stats(&self) -> Option<crate::SharedSocketStats> {
		None
	}

	/// Where the session's request target travels. Most transports carry no request URI,
	/// so the default is [`Address::Setup`](moq_net::transport::Address::Setup).
	fn address(&self) -> moq_net::transport::Address {
		moq_net::transport::Address::Setup
	}
}

#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
mod stream;
#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
pub use stream::Stream;
#[cfg(any(feature = "tcp", all(unix, feature = "uds")))]
pub(crate) use stream::build as build_stream_session;

#[cfg(feature = "ws")]
mod websocket;
// Public for `Driver<WsTransport<_>>`, though callers never need to name it.
#[cfg(feature = "ws")]
pub(crate) use websocket::WsTransport;
