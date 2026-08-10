//! In-memory mock WebTransport session for deterministic testing.
//!
//! Two `MockSession` instances form a bidirectional pair: streams opened on one
//! side appear as accepted streams on the other. Backed by kio queues so
//! delivery is ordered per-stream and deterministic (no real network jitter),
//! and so the mock implements the poll interface directly, which is what
//! moq-net requires.
//!
//! The mock guarantees that data written and FIN'd on a stream before `close()`
//! is readable by the peer, eliminating the Quinn CONNECTION_CLOSE race that
//! plagues real-transport tests.

use std::{
	sync::{Arc, Mutex},
	task::{Context, Poll},
};

use bytes::Bytes;
use web_transport_trait::poll;

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
/// wakes; the send side's `poll_closed` reads this without consuming state.
#[derive(Default)]
struct ClosedSignal {
	/// Set once the peer signals stop or drops.
	result: Mutex<Option<Result<(), MockError>>>,
	/// Wakes pending `poll_closed` watches.
	waiters: kio::Fan,
}

impl ClosedSignal {
	fn set(&self, result: Result<(), MockError>) {
		let mut slot = self.result.lock().unwrap();
		if slot.is_none() {
			*slot = Some(result);
			self.waiters.wake();
		}
	}
}

/// A mock send stream backed by a queue to the peer's reader.
pub struct MockSendStream {
	tx: Option<kio::Queue<StreamChunk>>,
	closed: Arc<ClosedSignal>,
	park: kio::Park,
}

impl poll::SendStream for MockSendStream {
	type Error = MockError;

	fn poll_write(&mut self, _cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		let Some(tx) = self.tx.as_ref() else {
			return Poll::Ready(Err(MockError::closed()));
		};
		match tx.try_push(StreamChunk::Data(Bytes::copy_from_slice(buf))) {
			Ok(()) => Poll::Ready(Ok(buf.len())),
			Err(_) => Poll::Ready(Err(MockError::closed())),
		}
	}

	fn set_priority(&mut self, _order: u8) {}

	fn finish(&mut self) -> Result<(), Self::Error> {
		if let Some(tx) = self.tx.take() {
			let _ = tx.try_push(StreamChunk::Fin);
		}
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		if let Some(tx) = self.tx.take() {
			let _ = tx.try_push(StreamChunk::Reset(code));
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		// Register before checking so a signal racing this poll still wakes it.
		self.closed.waiters.register(self.park.hold(cx));
		match self.closed.result.lock().unwrap().clone() {
			Some(result) => Poll::Ready(result),
			None => Poll::Pending,
		}
	}
}

impl Drop for MockSendStream {
	fn drop(&mut self) {
		// Dropped without an explicit FIN: deliver an implicit one, matching a
		// sender that went away cleanly.
		if let Some(tx) = self.tx.take() {
			let _ = tx.try_push(StreamChunk::Fin);
		}
	}
}

// ── RecvStream ──────────────────────────────────────────────────────

/// A mock receive stream backed by a queue from the peer's writer.
pub struct MockRecvStream {
	rx: kio::Queue<StreamChunk>,
	/// Buffered bytes from a chunk that was partially consumed.
	buf: Bytes,
	/// Whether we hit FIN or reset.
	done: bool,
	/// Shared signal to notify the peer's send-side `poll_closed`.
	closed: Arc<ClosedSignal>,
	park: kio::Park,
}

impl MockRecvStream {
	/// Pop the next chunk, mapping queue closure to an implicit FIN.
	fn poll_chunk(&mut self, cx: &mut Context<'_>) -> Poll<Option<StreamChunk>> {
		let waiter = self.park.hold(cx);
		match self.rx.poll_pop(waiter) {
			Poll::Ready(Ok(chunk)) => Poll::Ready(Some(chunk)),
			Poll::Ready(Err(_)) => Poll::Ready(None),
			Poll::Pending => Poll::Pending,
		}
	}
}

impl poll::RecvStream for MockRecvStream {
	type Error = MockError;

	fn poll_read(&mut self, cx: &mut Context<'_>, dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		if self.done {
			return Poll::Ready(Ok(None));
		}

		// Drain buffered bytes first.
		if !self.buf.is_empty() {
			let n = dst.len().min(self.buf.len());
			dst[..n].copy_from_slice(&self.buf[..n]);
			self.buf = self.buf.slice(n..);
			return Poll::Ready(Ok(Some(n)));
		}

		match std::task::ready!(self.poll_chunk(cx)) {
			Some(StreamChunk::Data(data)) => {
				let n = dst.len().min(data.len());
				dst[..n].copy_from_slice(&data[..n]);
				if n < data.len() {
					self.buf = data.slice(n..);
				}
				Poll::Ready(Ok(Some(n)))
			}
			Some(StreamChunk::Fin) | None => {
				self.done = true;
				Poll::Ready(Ok(None))
			}
			Some(StreamChunk::Reset(code)) => {
				self.done = true;
				Poll::Ready(Err(MockError::stream_reset(code)))
			}
		}
	}

	fn stop(&mut self, _code: u32) {
		self.closed.set(Ok(()));
		self.done = true;
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		if self.done {
			return Poll::Ready(Ok(()));
		}
		// Drain until done.
		loop {
			match std::task::ready!(self.poll_chunk(cx)) {
				Some(StreamChunk::Data(_)) => {}
				Some(StreamChunk::Fin) | None => {
					self.done = true;
					return Poll::Ready(Ok(()));
				}
				Some(StreamChunk::Reset(code)) => {
					self.done = true;
					return Poll::Ready(Err(MockError::stream_reset(code)));
				}
			}
		}
	}
}

impl Drop for MockRecvStream {
	fn drop(&mut self) {
		// Signal the paired SendStream that the receiver is gone (implicit STOP),
		// and fail its future writes.
		self.closed.set(Ok(()));
		self.rx.close();
	}
}

// ── Stream pair constructor ─────────────────────────────────────────

/// Create a linked (send, recv) stream pair.
fn new_stream_pair() -> (MockSendStream, MockRecvStream) {
	let queue = kio::Queue::new();
	let closed = Arc::new(ClosedSignal::default());

	let send = MockSendStream {
		tx: Some(queue.clone()),
		closed: closed.clone(),
		park: kio::Park::default(),
	};
	let recv = MockRecvStream {
		rx: queue,
		buf: Bytes::new(),
		done: false,
		closed,
		park: kio::Park::default(),
	};
	(send, recv)
}

// ── MockSession ─────────────────────────────────────────────────────

/// Connection-level state shared by both sides of a mock session pair.
///
/// A real QUIC CONNECTION_CLOSE tears down the entire connection for both peers.
/// This struct models that: a close on either side is visible to both.
#[derive(Default)]
struct ConnectionState {
	/// Set once by whichever side closes first.
	close_state: Mutex<Option<(u32, String)>>,
	/// Wakes both sides when close_state is populated.
	waiters: kio::Fan,
}

/// Per-side state: stream queues and a reference to the shared connection.
struct SessionSide {
	/// Bidi streams opened by the peer, popped by accept_bi.
	bidi: kio::Queue<(MockSendStream, MockRecvStream)>,
	/// Uni streams opened by the peer, popped by accept_uni.
	uni: kio::Queue<MockRecvStream>,
	/// Queue to deliver bidi streams TO the peer (its accept_bi pops them).
	peer_bidi: kio::Queue<(MockSendStream, MockRecvStream)>,
	/// Queue to deliver uni streams TO the peer (its accept_uni pops them).
	peer_uni: kio::Queue<MockRecvStream>,
	/// The ALPN protocol string for this side.
	protocol: Option<&'static str>,
	/// Connection-level close state shared with the peer.
	conn: Arc<ConnectionState>,
}

/// An in-memory mock WebTransport session.
///
/// Implements [`web_transport_trait::poll::Session`]. Created in pairs via
/// [`create_mock_session_pair`]. Streams opened on one side are delivered to
/// the peer's accept methods deterministically via unbounded queues.
#[derive(Clone)]
pub struct MockSession {
	side: Arc<SessionSide>,
	// One park per pending-operation class: a park holds a single waiter, and a
	// clone polling two classes at once must not clobber its own registration.
	accept_uni: kio::Park,
	accept_bi: kio::Park,
	closed: kio::Park,
}

impl poll::Session for MockSession {
	type SendStream = MockSendStream;
	type RecvStream = MockRecvStream;
	type Error = MockError;

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		// Register on the close signal too, so a close with no incoming streams
		// still fails the accept instead of parking it forever.
		let waiter = self.accept_uni.hold(cx);
		self.side.conn.waiters.register(waiter);
		if let Poll::Ready(res) = self.side.uni.poll_pop(waiter) {
			return Poll::Ready(res.map_err(|_| self.close_error()));
		}
		// Bind the flag: a guard living through the match would deadlock against
		// close_error taking the same lock.
		let closed = self.side.conn.close_state.lock().unwrap().is_some();
		match closed {
			true => Poll::Ready(Err(self.close_error())),
			false => Poll::Pending,
		}
	}

	fn poll_accept_bi(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let waiter = self.accept_bi.hold(cx);
		self.side.conn.waiters.register(waiter);
		if let Poll::Ready(res) = self.side.bidi.poll_pop(waiter) {
			return Poll::Ready(res.map_err(|_| self.close_error()));
		}
		// Bind the flag: a guard living through the match would deadlock against
		// close_error taking the same lock.
		let closed = self.side.conn.close_state.lock().unwrap().is_some();
		match closed {
			true => Poll::Ready(Err(self.close_error())),
			false => Poll::Pending,
		}
	}

	fn poll_open_bi(
		&mut self,
		_cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		// Create two stream pairs: one for each direction.
		let (our_send, peer_recv) = new_stream_pair();
		let (peer_send, our_recv) = new_stream_pair();

		// Deliver (peer_send, peer_recv) to the peer's accept_bi.
		match self.side.peer_bidi.try_push((peer_send, peer_recv)) {
			Ok(()) => Poll::Ready(Ok((our_send, our_recv))),
			Err(_) => Poll::Ready(Err(self.close_error())),
		}
	}

	fn poll_open_uni(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		let (our_send, peer_recv) = new_stream_pair();

		// Deliver peer_recv to the peer's accept_uni.
		match self.side.peer_uni.try_push(peer_recv) {
			Ok(()) => Poll::Ready(Ok(our_send)),
			Err(_) => Poll::Ready(Err(self.close_error())),
		}
	}

	fn poll_send_datagram(&mut self, _cx: &mut Context<'_>, _payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		// Datagrams are best-effort; silently succeed in mock.
		Poll::Ready(Ok(()))
	}

	fn poll_recv_datagram(&mut self, _cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		// No datagram support in mock; pend forever.
		Poll::Pending
	}

	fn max_datagram_size(&self) -> usize {
		1200
	}

	fn protocol(&self) -> Option<&str> {
		self.side.protocol
	}

	fn close(&mut self, code: u32, reason: &str) {
		// Set-once: a real QUIC CONNECTION_CLOSE keeps the first reason, and the
		// GOAWAY-timeout tests rely on the server's force-close reason surviving
		// a later local teardown close from the peer's own driver.
		let mut state = self.side.conn.close_state.lock().unwrap();
		if state.is_none() {
			*state = Some((code, reason.to_string()));
			// Wake outside the lock: a woken task may poll straight back into it.
			drop(state);
			self.side.conn.waiters.wake();
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		// Register before checking so a close racing this poll still wakes it.
		self.side.conn.waiters.register(self.closed.hold(cx));
		let state = self.side.conn.close_state.lock().unwrap().clone();
		match state {
			Some((code, reason)) => Poll::Ready(MockError {
				code: Some(code),
				reason,
			}),
			None => Poll::Pending,
		}
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		web_transport_trait::StatsUnavailable
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
/// Both sides report the given `protocol` from
/// [`web_transport_trait::poll::Session::protocol`], matching ALPN negotiation
/// behavior.
pub fn create_mock_session_pair(protocol: Option<&'static str>) -> (MockSession, MockSession) {
	let conn = Arc::new(ConnectionState::default());

	// Queues for client -> server and server -> client stream delivery.
	let c2s_bidi = kio::Queue::new();
	let c2s_uni = kio::Queue::new();
	let s2c_bidi = kio::Queue::new();
	let s2c_uni = kio::Queue::new();

	let client_side = Arc::new(SessionSide {
		bidi: s2c_bidi.clone(),
		uni: s2c_uni.clone(),
		peer_bidi: c2s_bidi.clone(),
		peer_uni: c2s_uni.clone(),
		protocol,
		conn: conn.clone(),
	});

	let server_side = Arc::new(SessionSide {
		bidi: c2s_bidi,
		uni: c2s_uni,
		peer_bidi: s2c_bidi,
		peer_uni: s2c_uni,
		protocol,
		conn,
	});

	let new = |side| MockSession {
		side,
		accept_uni: kio::Park::default(),
		accept_bi: kio::Park::default(),
		closed: kio::Park::default(),
	};

	(new(client_side), new(server_side))
}
