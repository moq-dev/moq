//! In-memory mock WebTransport session for deterministic testing.
//!
//! Two `MockSession` instances form a bidirectional pair: streams opened on one
//! side appear as accepted streams on the other. Backed by tokio channels so
//! delivery is ordered per-stream and deterministic (no real network jitter).
//!
//! The mock guarantees that data written and FIN'd on a stream before `close()`
//! is readable by the peer, eliminating the Quinn CONNECTION_CLOSE race that
//! plagues real-transport tests.

use std::sync::{Arc, Mutex};

use bytes::Bytes;
use tokio::sync::mpsc;

// ── Error ───────────────────────────────────────────────────────────

/// Error type for mock transport operations.
#[derive(Debug, Clone)]
pub struct MockError {
	code: Option<u32>,
	reason: String,
}

impl MockError {
	fn closed() -> Self {
		Self {
			code: Some(0),
			reason: "session closed".into(),
		}
	}

	fn stream_reset(code: u32) -> Self {
		Self {
			code: Some(code),
			reason: "stream reset".into(),
		}
	}
}

impl std::fmt::Display for MockError {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "mock transport: {}", self.reason)
	}
}

impl std::error::Error for MockError {}

impl web_transport_trait::Error for MockError {
	fn session_error(&self) -> Option<(u32, String)> {
		self.code.map(|c| (c, self.reason.clone()))
	}

	fn stream_error(&self) -> Option<u32> {
		self.code
	}
}

// ── SendStream ──────────────────────────────────────────────────────

/// Internal chunk type: either data or a terminal signal.
enum StreamChunk {
	Data(Bytes),
	Fin,
	Reset(u32),
}

/// Shared closed-signal state between a paired SendStream and RecvStream.
///
/// Models QUIC STOP_SENDING: the recv side (or its Drop) flips the flag and
/// notifies; the send side's `closed()` polls this without consuming state.
struct ClosedSignal {
	/// Set once the peer signals stop or drops.
	result: Mutex<Option<Result<(), MockError>>>,
	/// Wakes pending `closed()` futures.
	notify: tokio::sync::Notify,
}

/// A mock send stream backed by an mpsc channel to the peer's reader.
pub struct MockSendStream {
	tx: Option<mpsc::UnboundedSender<StreamChunk>>,
	closed: Arc<ClosedSignal>,
}

impl web_transport_trait::SendStream for MockSendStream {
	type Error = MockError;

	async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
		let tx = self.tx.as_ref().ok_or_else(MockError::closed)?;
		tx.send(StreamChunk::Data(Bytes::copy_from_slice(buf)))
			.map_err(|_| MockError::closed())?;
		Ok(buf.len())
	}

	fn set_priority(&mut self, _order: u8) {}

	fn finish(&mut self) -> Result<(), Self::Error> {
		if let Some(tx) = self.tx.take() {
			let _ = tx.send(StreamChunk::Fin);
		}
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		if let Some(tx) = self.tx.take() {
			let _ = tx.send(StreamChunk::Reset(code));
		}
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		loop {
			// Check if already signaled (idempotent: never consumes state).
			let notified = self.closed.notify.notified();
			if let Some(result) = self.closed.result.lock().unwrap().clone() {
				return result;
			}
			notified.await;
		}
	}
}

// ── RecvStream ──────────────────────────────────────────────────────

/// A mock receive stream backed by an mpsc channel from the peer's writer.
pub struct MockRecvStream {
	rx: mpsc::UnboundedReceiver<StreamChunk>,
	/// Buffered bytes from a chunk that was partially consumed.
	buf: Bytes,
	/// Whether we hit FIN or reset.
	done: bool,
	/// Shared signal to notify the peer's SendStream::closed().
	closed: Arc<ClosedSignal>,
}

impl web_transport_trait::RecvStream for MockRecvStream {
	type Error = MockError;

	async fn read(&mut self, dst: &mut [u8]) -> Result<Option<usize>, Self::Error> {
		if self.done {
			return Ok(None);
		}

		// Drain buffered bytes first.
		if !self.buf.is_empty() {
			let n = dst.len().min(self.buf.len());
			dst[..n].copy_from_slice(&self.buf[..n]);
			self.buf = self.buf.slice(n..);
			return Ok(Some(n));
		}

		// Wait for next chunk from peer.
		match self.rx.recv().await {
			Some(StreamChunk::Data(data)) => {
				let n = dst.len().min(data.len());
				dst[..n].copy_from_slice(&data[..n]);
				if n < data.len() {
					self.buf = data.slice(n..);
				}
				Ok(Some(n))
			}
			Some(StreamChunk::Fin) => {
				self.done = true;
				Ok(None)
			}
			Some(StreamChunk::Reset(code)) => {
				self.done = true;
				Err(MockError::stream_reset(code))
			}
			None => {
				// Sender dropped without explicit FIN: treat as implicit FIN.
				self.done = true;
				Ok(None)
			}
		}
	}

	fn stop(&mut self, _code: u32) {
		let mut result = self.closed.result.lock().unwrap();
		if result.is_none() {
			*result = Some(Ok(()));
			self.closed.notify.notify_waiters();
		}
		self.done = true;
	}

	async fn closed(&mut self) -> Result<(), Self::Error> {
		if self.done {
			return Ok(());
		}
		// Drain until done.
		loop {
			match self.rx.recv().await {
				Some(StreamChunk::Data(_)) => continue,
				Some(StreamChunk::Fin) | None => {
					self.done = true;
					return Ok(());
				}
				Some(StreamChunk::Reset(code)) => {
					self.done = true;
					return Err(MockError::stream_reset(code));
				}
			}
		}
	}
}

impl Drop for MockRecvStream {
	fn drop(&mut self) {
		// Signal the paired SendStream that the receiver is gone (implicit STOP).
		let mut result = self.closed.result.lock().unwrap();
		if result.is_none() {
			*result = Some(Ok(()));
			self.closed.notify.notify_waiters();
		}
	}
}

// ── Stream pair constructor ─────────────────────────────────────────

/// Create a linked (send, recv) stream pair.
fn new_stream_pair() -> (MockSendStream, MockRecvStream) {
	let (data_tx, data_rx) = mpsc::unbounded_channel();

	let closed = Arc::new(ClosedSignal {
		result: Mutex::new(None),
		notify: tokio::sync::Notify::new(),
	});

	let send = MockSendStream {
		tx: Some(data_tx),
		closed: closed.clone(),
	};
	let recv = MockRecvStream {
		rx: data_rx,
		buf: Bytes::new(),
		done: false,
		closed,
	};
	(send, recv)
}

// ── MockSession ─────────────────────────────────────────────────────

/// Connection-level state shared by both sides of a mock session pair.
///
/// A real QUIC CONNECTION_CLOSE tears down the entire connection for both peers.
/// This struct models that: a close on either side is visible to both.
struct ConnectionState {
	/// Set once by whichever side closes first.
	close_state: Mutex<Option<(u32, String)>>,
	/// Wakes both sides when close_state is populated.
	close_notify: tokio::sync::Notify,
}

/// Per-side state: stream channels and a reference to the shared connection.
struct SessionSide {
	/// Bidi streams opened by the peer, available via accept_bi.
	bidi_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<(MockSendStream, MockRecvStream)>>,
	/// Uni streams opened by the peer, available via accept_uni.
	uni_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<MockRecvStream>>,
	/// Channel to send bidi streams TO the peer (peer's accept_bi picks them up).
	peer_bidi_tx: mpsc::UnboundedSender<(MockSendStream, MockRecvStream)>,
	/// Channel to send uni streams TO the peer (peer's accept_uni picks them up).
	peer_uni_tx: mpsc::UnboundedSender<MockRecvStream>,
	/// The ALPN protocol string for this side.
	protocol: Option<&'static str>,
	/// Connection-level close state shared with the peer.
	conn: Arc<ConnectionState>,
}

/// An in-memory mock WebTransport session.
///
/// Implements [`web_transport_trait::Session`]. Created in pairs via
/// [`create_mock_session_pair`]. Streams opened on one side are delivered to
/// the peer's accept methods deterministically via unbounded channels.
#[derive(Clone)]
pub struct MockSession {
	side: Arc<SessionSide>,
}

impl web_transport_trait::Session for MockSession {
	type SendStream = MockSendStream;
	type RecvStream = MockRecvStream;
	type Error = MockError;

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		let mut rx = self.side.uni_rx.lock().await;
		match rx.recv().await {
			Some(stream) => Ok(stream),
			None => {
				// Channel closed: session is shutting down. Wait for close signal.
				self.side.conn.close_notify.notified().await;
				Err(self.close_error())
			}
		}
	}

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		let mut rx = self.side.bidi_rx.lock().await;
		match rx.recv().await {
			Some(pair) => Ok(pair),
			None => {
				self.side.conn.close_notify.notified().await;
				Err(self.close_error())
			}
		}
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		// Create two stream pairs: one for each direction.
		let (our_send, peer_recv) = new_stream_pair();
		let (peer_send, our_recv) = new_stream_pair();

		// Deliver (peer_send, peer_recv) to the peer's accept_bi.
		self.side
			.peer_bidi_tx
			.send((peer_send, peer_recv))
			.map_err(|_| self.close_error())?;

		Ok((our_send, our_recv))
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		let (our_send, peer_recv) = new_stream_pair();

		// Deliver peer_recv to the peer's accept_uni.
		self.side.peer_uni_tx.send(peer_recv).map_err(|_| self.close_error())?;

		Ok(our_send)
	}

	fn send_datagram(&self, _payload: Bytes) -> Result<(), Self::Error> {
		// Datagrams are best-effort; silently succeed in mock.
		Ok(())
	}

	async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
		// No datagram support in mock; pend forever.
		std::future::pending().await
	}

	fn max_datagram_size(&self) -> usize {
		1200
	}

	fn protocol(&self) -> Option<&str> {
		self.side.protocol
	}

	fn close(&self, code: u32, reason: &str) {
		*self.side.conn.close_state.lock().unwrap() = Some((code, reason.to_string()));
		self.side.conn.close_notify.notify_waiters();
	}

	async fn closed(&self) -> Self::Error {
		loop {
			let notified = self.side.conn.close_notify.notified();
			if let Some((code, reason)) = self.side.conn.close_state.lock().unwrap().clone() {
				return MockError {
					code: Some(code),
					reason,
				};
			}
			notified.await;
		}
	}
}

impl MockSession {
	fn close_error(&self) -> MockError {
		self.side
			.conn
			.close_state
			.lock()
			.unwrap()
			.as_ref()
			.map(|(code, reason)| MockError {
				code: Some(*code),
				reason: reason.clone(),
			})
			.unwrap_or_else(MockError::closed)
	}
}

// ── Pair constructor ────────────────────────────────────────────────

/// Create a pair of connected mock sessions.
///
/// Streams opened on `client` appear in `server.accept_*()` and vice versa.
/// Both sides report the given `protocol` from [`web_transport_trait::Session::protocol`],
/// matching ALPN negotiation behavior.
pub fn create_mock_session_pair(protocol: Option<&'static str>) -> (MockSession, MockSession) {
	let conn = Arc::new(ConnectionState {
		close_state: Mutex::new(None),
		close_notify: tokio::sync::Notify::new(),
	});

	// Channels for client -> server stream delivery.
	let (c2s_bidi_tx, c2s_bidi_rx) = mpsc::unbounded_channel();
	let (c2s_uni_tx, c2s_uni_rx) = mpsc::unbounded_channel();

	// Channels for server -> client stream delivery.
	let (s2c_bidi_tx, s2c_bidi_rx) = mpsc::unbounded_channel();
	let (s2c_uni_tx, s2c_uni_rx) = mpsc::unbounded_channel();

	let client_side = Arc::new(SessionSide {
		bidi_rx: tokio::sync::Mutex::new(s2c_bidi_rx),
		uni_rx: tokio::sync::Mutex::new(s2c_uni_rx),
		peer_bidi_tx: c2s_bidi_tx,
		peer_uni_tx: c2s_uni_tx,
		protocol,
		conn: conn.clone(),
	});

	let server_side = Arc::new(SessionSide {
		bidi_rx: tokio::sync::Mutex::new(c2s_bidi_rx),
		uni_rx: tokio::sync::Mutex::new(c2s_uni_rx),
		peer_bidi_tx: s2c_bidi_tx,
		peer_uni_tx: s2c_uni_tx,
		protocol,
		conn,
	});

	let client = MockSession { side: client_side };
	let server = MockSession { side: server_side };

	(client, server)
}
