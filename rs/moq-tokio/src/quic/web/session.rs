use std::{
	fmt,
	future::Future,
	io::Cursor,
	ops::Deref,
	pin::Pin,
	sync::{Arc, Mutex, OnceLock},
	task::{Context, Poll, Waker},
	time::Duration,
};

use bytes::{Bytes, BytesMut};
use futures::stream::{FuturesUnordered, Stream, StreamExt};
use kio::Waiter;

use crate::quic::web::fan::Fan;
use crate::quic::web::{
	ClientError, CloseReason, Connected, RecvStream, SendStream, SessionError, Settings, WebTransportError,
	proto::{ConnectRequest, ConnectResponse, Frame, StreamUni, VarInt},
};

/// The ALPN the QUIC handshake negotiated, or `None` if there was none, the handshake
/// has not completed, or it is not valid UTF-8.
///
/// The driver only exposes the ALPN via a downcast of the boxed handshake data, so the result
/// is owned rather than borrowed from the connection.
#[cfg(any(feature = "aws-lc-rs", feature = "ring"))]
fn negotiated_alpn(conn: &crate::quic::driver::Connection) -> Option<String> {
	let data = conn.handshake_data()?;
	let data = data
		.downcast::<crate::quic::driver::crypto::rustls::HandshakeData>()
		.ok()?;

	String::from_utf8(data.protocol?).ok()
}

/// `crate::quic::driver::crypto::rustls` only exists once a TLS backend is enabled, so there is no
/// handshake data type to name here.
///
/// This crate's own builders are gated the same way, so normally there is no connection
/// to read an ALPN from either. A caller that enables a provider on `moq-quic` directly and
/// brings its own endpoint can still establish one, and [`Session::protocol`] reports
/// `None` for those raw sessions. Enable this crate's `aws-lc-rs` or `ring` feature to
/// get the ALPN.
#[cfg(not(any(feature = "aws-lc-rs", feature = "ring")))]
fn negotiated_alpn(_conn: &crate::quic::driver::Connection) -> Option<String> {
	None
}

/// An established WebTransport session, acting like a full QUIC connection. See
/// [`crate::quic::driver::Connection`].
///
/// It is important to remember that WebTransport is layered on top of QUIC:
///   1. Each stream starts with a few bytes identifying the stream type and session ID.
///   2. Errors codes are encoded with the session ID, so they aren't full QUIC error codes.
///   3. Stream IDs may have gaps in them, used by HTTP/3 transparent to the application.
///
/// Deref is used to expose non-overloaded methods on [`crate::quic::driver::Connection`].
/// These should be safe to use with WebTransport, but file a PR if you find one that isn't.
#[derive(Clone)]
pub struct Session {
	conn: crate::quic::driver::Connection,

	// The session ID, as determined by the stream ID of the connect request.
	session_id: Option<VarInt>,

	// The accept logic is stateful, so use an Arc<Mutex> to share it.
	accept: Option<Arc<Mutex<SessionAccept>>>,

	// Cache the headers in front of each stream we open.
	header_uni: Vec<u8>,
	header_bi: Vec<u8>,
	header_datagram: Vec<u8>,

	// Keep a reference to the settings and connect stream to avoid closing them until dropped.
	#[allow(dead_code)]
	settings: Option<Arc<Settings>>,

	// The send side of the CONNECT stream, used to write the CloseWebTransportSession capsule.
	// Wrapped in Arc<Mutex<Option<...>>> so close() can take it exactly once.
	connect_send: Arc<Mutex<Option<crate::quic::driver::SendStream>>>,

	// Why the session closed: set once by a local close() or the background task
	// when a remote CloseWebTransportSession capsule is received.
	close: Arc<CloseReason>,

	// The request sent by the client, or None for a raw QUIC session.
	request: Option<ConnectRequest>,

	// The response sent by the server, or None for a raw QUIC session.
	response: Option<ConnectResponse>,

	// The ALPN negotiated by the QUIC handshake, for raw QUIC sessions. The driver only
	// exposes it behind an allocating downcast, so `protocol()` has nothing in the
	// connection to borrow from and needs somewhere to keep the result.
	//
	// Resolved on first read rather than at construction: `raw()` accepts any
	// connection, including one whose handshake has not completed and so has no ALPN
	// yet. Only a successful read is latched, so an early call cannot pin `None` for
	// the life of the session. Shared across clones.
	//
	// Unused by HTTP/3 sessions, which read the subprotocol out of the response.
	alpn: Arc<OnceLock<String>>,
}

impl Session {
	pub(crate) fn new(conn: crate::quic::driver::Connection, settings: Settings, connect: Connected) -> Self {
		// The session ID is the stream ID of the CONNECT request.
		let session_id = connect.session_id();

		// Cache the tiny header we write in front of each stream we open.
		let mut header_uni = Vec::new();
		StreamUni::WEBTRANSPORT.encode(&mut header_uni);
		session_id.encode(&mut header_uni);

		let mut header_bi = Vec::new();
		Frame::WEBTRANSPORT.encode(&mut header_bi);
		session_id.encode(&mut header_bi);

		let mut header_datagram = Vec::new();
		session_id.encode(&mut header_datagram);

		let close = Arc::new(CloseReason::new(false));

		// Accept logic is stateful, so use an Arc<Mutex> to share it.
		let accept = SessionAccept::new(conn.clone(), session_id, close.clone());

		let this = Self {
			conn,
			accept: Some(Arc::new(Mutex::new(accept))),
			session_id: Some(session_id),
			header_uni,
			header_bi,
			header_datagram,
			settings: Some(Arc::new(settings)),
			connect_send: Arc::new(Mutex::new(Some(connect.send))),
			close: close.clone(),
			request: Some(connect.request.clone()),
			response: Some(connect.response.clone()),
			alpn: Default::default(),
		};

		// Run a background task to read capsules from the CONNECT recv stream.
		let conn2 = this.conn.clone();
		tokio::spawn(Self::run_recv(conn2, connect.recv, close));

		this
	}

	// Read capsules from the CONNECT recv stream until it's closed,
	// then record the close error and tear down the connection.
	async fn run_recv(
		conn: crate::quic::driver::Connection,
		recv: crate::quic::driver::RecvStream,
		close: Arc<CloseReason>,
	) {
		let close_info = Self::read_capsules(&conn, recv).await;
		let code = close_info.as_ref().map_or(0, |(c, _)| *c);

		let http3_code: crate::quic::driver::VarInt = web_transport_proto::error_to_http3(code).try_into().unwrap();

		// Try to record the remote close error. If close() already set
		// the error, it owns the connection teardown, so we bail out.
		match close_info {
			Some((code, reason)) => {
				let err = WebTransportError::Closed(code, reason.clone());
				if !close.set(err.into()) {
					return;
				}
				conn.close(http3_code, reason.as_bytes());
			}
			None => {
				let err = crate::quic::driver::ConnectionError::LocallyClosed.into();
				if !close.set(err) {
					return;
				}
				conn.close(http3_code, b"");
			}
		};
	}

	// Keep reading capsules from the CONNECT recv stream until it's closed.
	// Returns Some((code, reason)) if a CloseWebTransportSession capsule was received,
	// or None if the stream closed without a capsule.
	async fn read_capsules(
		conn: &crate::quic::driver::Connection,
		recv: crate::quic::driver::RecvStream,
	) -> Option<(u32, String)> {
		let mut reader = web_transport_proto::Http3CapsuleReader::new(recv);
		loop {
			match reader.read().await {
				Ok(Some(web_transport_proto::Capsule::CloseWebTransportSession { code, reason })) => {
					return Some((code, reason));
				}
				Ok(Some(web_transport_proto::Capsule::Grease { .. })) => {}
				Ok(Some(web_transport_proto::Capsule::Unknown { typ, payload })) => {
					tracing::warn!(%typ, size = payload.len(), "unknown capsule");
				}
				Ok(None) => return None,
				// The capsule reader reports a lost stream as `UnexpectedEnd`, so ask the
				// connection: a read cut short by its close is the ordinary teardown.
				Err(e) => {
					match conn.close_reason() {
						Some(reason) => tracing::debug!(?e, %reason, "CONNECT stream ended with the connection"),
						None => tracing::warn!(?e, "failed to read capsule"),
					}
					return None;
				}
			}
		}
	}

	/// Connect using an established QUIC connection if you want to create the connection yourself.
	/// This will only work with a brand new QUIC connection using the HTTP/3 ALPN.
	pub async fn connect(
		conn: crate::quic::driver::Connection,
		request: impl Into<ConnectRequest>,
	) -> Result<Session, ClientError> {
		let request = request.into();

		// Perform the H3 handshake by sending/receiving SETTINGS frames.
		let settings = Settings::connect(&conn).await?;

		// Send the HTTP/3 CONNECT request.
		let connect = Connected::open(&conn, request).await?;

		// Return the resulting session with a reference to the control/connect streams.
		// If either stream is closed, then the session will be closed, so we need to keep them
		// around.
		let session = Session::new(conn, settings, connect);

		Ok(session)
	}

	/// Accept a new unidirectional stream. See [`crate::quic::driver::Connection::accept_uni`].
	pub async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
		if let Some(accept) = &self.accept {
			// `kio::wait` owns the waiter, so dropping this future — a `timeout` that
			// expires, say — also drops its registration in `SessionAccept`.
			kio::wait(|waiter| poll_accept_uni_shared(accept, waiter))
				.await
				.map_err(|e| self.map_error(e))
		} else {
			let recv = self.conn.accept_uni().await.map_err(|e| self.map_error(e))?;
			Ok(RecvStream::new(recv, self.close.clone()))
		}
	}

	/// Accept a new bidirectional stream. See [`crate::quic::driver::Connection::accept_bi`].
	pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
		if let Some(accept) = &self.accept {
			kio::wait(|waiter| poll_accept_bi_shared(accept, waiter))
				.await
				.map_err(|e| self.map_error(e))
		} else {
			let (send, recv) = self.conn.accept_bi().await.map_err(|e| self.map_error(e))?;
			Ok((
				SendStream::new(send, self.close.clone(), None),
				RecvStream::new(recv, self.close.clone()),
			))
		}
	}

	/// Open a new unidirectional stream. See [`crate::quic::driver::Connection::open_uni`].
	pub async fn open_uni(&self) -> Result<SendStream, SessionError> {
		let mut send = self.conn.open_uni().await.map_err(|e| self.map_error(e))?;

		// Set the stream priority to max and then write the stream header.
		// Otherwise the application could write data with lower priority than the header, resulting
		// in queuing. Also the header is very important for determining the session ID
		// without reliable reset.
		send.set_priority(i32::MAX).ok();
		Self::write_full(&mut send, &self.header_uni)
			.await
			.map_err(|e| self.map_error(e))?;

		// Reset the stream priority back to the default of 0.
		send.set_priority(0).ok();
		let header = send.checkpoint();
		Ok(SendStream::new(send, self.close.clone(), Some(header)))
	}

	/// Open a new bidirectional stream. See [`crate::quic::driver::Connection::open_bi`].
	pub async fn open_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
		let (mut send, recv) = self.conn.open_bi().await.map_err(|e| self.map_error(e))?;

		// Set the stream priority to max and then write the stream header.
		// Otherwise the application could write data with lower priority than the header, resulting
		// in queuing. Also the header is very important for determining the session ID
		// without reliable reset.
		send.set_priority(i32::MAX).ok();
		Self::write_full(&mut send, &self.header_bi)
			.await
			.map_err(|e| self.map_error(e))?;

		// Reset the stream priority back to the default of 0.
		send.set_priority(0).ok();
		let header = send.checkpoint();
		Ok((
			SendStream::new(send, self.close.clone(), Some(header)),
			RecvStream::new(recv, self.close.clone()),
		))
	}

	/// Asynchronously receives an application datagram from the remote peer.
	///
	/// This method is used to receive an application datagram sent by the remote
	/// peer over the connection.
	/// It waits for a datagram to become available and returns the received bytes.
	pub async fn read_datagram(&self) -> Result<Bytes, SessionError> {
		let mut datagram = self.conn.read_datagram().await.map_err(|e| self.map_error(e))?;

		let mut cursor = Cursor::new(&datagram);

		if let Some(session_id) = self.session_id {
			// We have to check and strip the session ID from the datagram.
			let actual_id = VarInt::decode(&mut cursor).map_err(|_| WebTransportError::UnknownSession)?;
			if actual_id != session_id {
				return Err(WebTransportError::UnknownSession.into());
			}
		}

		// Return the datagram without the session ID.
		let datagram = datagram.split_off(cursor.position() as usize);

		Ok(datagram)
	}

	/// Sends an application datagram to the remote peer.
	///
	/// Datagrams are unreliable and may be dropped or delivered out of order.
	/// The data must be smaller than [`max_datagram_size`](Self::max_datagram_size).
	pub fn send_datagram(&self, data: Bytes) -> Result<(), SessionError> {
		let result = if !self.header_datagram.is_empty() {
			// Unfortunately, we need to allocate/copy each datagram because of the driver API.
			// Pls go +1 if you care: https://github.com/quinn-rs/quinn/issues/1724
			let mut buf = BytesMut::with_capacity(self.header_datagram.len() + data.len());

			// Prepend the datagram with the header indicating the session ID.
			buf.extend_from_slice(&self.header_datagram);
			buf.extend_from_slice(&data);

			self.conn.send_datagram(buf.into())
		} else {
			self.conn.send_datagram(data)
		};

		result.map_err(|e| self.map_error(e))?;
		Ok(())
	}

	/// Sends an application datagram to the remote peer, waiting for buffer space if needed.
	///
	/// Datagrams are unreliable and may be dropped or delivered out of order.
	/// The data must be smaller than [`max_datagram_size`](Self::max_datagram_size).
	pub async fn send_datagram_wait(&self, data: Bytes) -> Result<(), SessionError> {
		let result = if !self.header_datagram.is_empty() {
			// Unfortunately, we need to allocate/copy each datagram because of the driver API.
			// Pls go +1 if you care: https://github.com/quinn-rs/quinn/issues/1724
			let mut buf = BytesMut::with_capacity(self.header_datagram.len() + data.len());

			// Prepend the datagram with the header indicating the session ID.
			buf.extend_from_slice(&self.header_datagram);
			buf.extend_from_slice(&data);

			self.conn.send_datagram_wait(buf.into()).await
		} else {
			self.conn.send_datagram_wait(data).await
		};

		result.map_err(|e| self.map_error(e))?;
		Ok(())
	}

	/// Computes the maximum size of datagrams that may be passed to
	/// [`send_datagram`](Self::send_datagram), or 0 when the peer does not accept datagrams.
	pub fn max_datagram_size(&self) -> usize {
		self.conn
			.max_datagram_size()
			.unwrap_or(0)
			.saturating_sub(self.header_datagram.len())
	}

	/// Returns the available buffer space for sending datagrams.
	///
	/// This value is adjusted to account for the WebTransport session header.
	pub fn datagram_send_buffer_space(&self) -> usize {
		self.conn
			.datagram_send_buffer_space()
			.saturating_sub(self.header_datagram.len())
	}

	/// Close the session with an error code and reason.
	///
	/// When there is a session ID (WebTransport over HTTP/3), a `CloseWebTransportSession`
	/// capsule is written on the CONNECT stream before the QUIC connection is closed.
	/// This allows browser clients to receive the close code and reason via `WebTransport.closed`.
	///
	/// The capsule write and connection close happen asynchronously in a spawned task
	/// that keeps the session alive until then, so the caller may drop it right away.
	/// `await` [`Session::closed()`] to learn when the connection is gone. Session
	/// operations will fail once the QUIC connection is closed.
	pub fn close(&self, code: u32, reason: &[u8]) {
		// First close wins: a no-op if the connection already closed, the background
		// task recorded a remote close, or close() was already called.
		if let Some(err) = self.conn.close_reason() {
			self.close.set(self.map_error(err));
			return;
		}
		let err = SessionError::ConnectionError(crate::quic::driver::ConnectionError::LocallyClosed);
		if !self.close.set(err) {
			return;
		}

		if self.session_id.is_some() {
			// Take the send stream for the capsule write.
			let send = self.connect_send.lock().unwrap().take();

			if let Some(send) = send {
				let reason = String::from_utf8_lossy(reason).into_owned();
				let capsule = web_transport_proto::Capsule::CloseWebTransportSession { code, reason };
				let timeout = (self.conn.rtt() * 3).max(Duration::from_millis(100));

				// The task owns a clone of the whole session, not just the connection:
				// dropping the caller's last handle would otherwise end the HTTP/3
				// control and QPACK streams before the capsule is delivered, which a
				// browser treats as a connection error, losing the code and reason.
				let session = self.clone();
				tokio::spawn(async move {
					session.close_with_capsule(send, capsule, code, timeout).await;
				});
			}
		} else {
			// Raw QUIC mode: no capsule needed.
			self.conn.close(code.into(), reason);
		}
	}

	/// Deliver the close capsule and FIN, then give the peer its teardown grace.
	async fn close_with_capsule(
		&self,
		mut send: crate::quic::driver::SendStream,
		capsule: web_transport_proto::Capsule,
		code: u32,
		timeout: std::time::Duration,
	) {
		let http3_code: crate::quic::driver::VarInt = web_transport_proto::error_to_http3(code).try_into().unwrap();

		// Encode the capsule, then wrap it in an HTTP/3 DATA frame.
		// In HTTP/3, capsule data is carried inside DATA frames on the CONNECT
		// stream (RFC 9297 Section 3.2).
		let mut capsule_bytes = Vec::new();
		capsule.encode(&mut capsule_bytes);

		let mut frame = Vec::new();
		Frame::DATA.encode(&mut frame);
		let Ok(len) = VarInt::try_from(capsule_bytes.len()) else {
			tracing::warn!("capsule too large to encode as DATA frame");
			self.conn.close(http3_code, b"");
			return;
		};
		len.encode(&mut frame);
		frame.extend_from_slice(&capsule_bytes);

		// Write the DATA frame to the CONNECT send stream.
		if let Err(e) = send.write_all(&frame).await {
			tracing::warn!(?e, "failed to write CloseWebTransportSession capsule");
			self.conn.close(http3_code, b"");
			return;
		}

		// FIN the send stream so the peer knows no more capsules are coming.
		if let Err(e) = send.finish() {
			tracing::warn!(?e, "failed to finish CONNECT send stream");
			self.conn.close(http3_code, b"");
			return;
		}

		// A queued write is not delivery: tearing down QUIC before the DATA and FIN
		// are acknowledged can discard the capsule during loss recovery. Transport
		// failure (including endpoint shutdown) also completes this wait.
		match send.stopped().await {
			Ok(None) => {}
			Ok(Some(code)) => {
				tracing::debug!(?code, "peer stopped the CONNECT stream during close");
				self.conn.close(http3_code, b"");
				return;
			}
			Err(e) => {
				tracing::debug!(?e, "connection ended before the close capsule was acknowledged");
				return;
			}
		}

		// Wait for the peer to close the CONNECT stream after receiving the capsule.
		if tokio::time::timeout(timeout, self.conn.closed()).await.is_err() {
			tracing::debug!("timeout waiting for peer to close; force-closing connection");
			self.conn.close(http3_code, b"");
		}
	}

	/// Wait until the session is closed, returning the error. See [`crate::quic::driver::Connection::closed`].
	///
	/// If the peer sent a `CloseWebTransportSession` capsule, the returned error will be
	/// [`WebTransportError::Closed`] with the code and reason from the capsule.
	///
	/// Unlike [`crate::quic::driver::Connection::closed`], this does **not** return early when
	/// [`close()`](Self::close) has been called. It waits for the underlying QUIC
	/// connection to shut down, ensuring the `CloseWebTransportSession` capsule has
	/// been delivered. Use [`close_reason()`](Self::close_reason) for a non-blocking check.
	pub async fn closed(&self) -> SessionError {
		self.map_error(self.conn.closed().await)
	}

	/// Return why the session was closed, or None if it's not closed. See
	/// [`crate::quic::driver::Connection::close_reason`].
	pub fn close_reason(&self) -> Option<SessionError> {
		self.conn.close_reason().map(|e| self.map_error(e))
	}

	/// Report an error caused by the connection closing as the session's close reason.
	fn map_error(&self, e: impl Into<SessionError>) -> SessionError {
		self.close.map(e.into())
	}

	async fn write_full(send: &mut crate::quic::driver::SendStream, buf: &[u8]) -> Result<(), SessionError> {
		match send.write_all(buf).await {
			Ok(_) => Ok(()),
			Err(crate::quic::driver::WriteError::ConnectionLost(err)) => Err(err.into()),
			Err(err) => Err(WebTransportError::WriteError(err).into()),
		}
	}

	/// Create a new session from a raw QUIC connection.
	///
	/// This is used to pretend like a QUIC connection is a WebTransport session,
	/// making it easier to support WebTransport and raw QUIC simultaneously.
	///
	/// There is no HTTP/3 exchange, so [`Self::request`] and [`Self::response`] both
	/// return `None`. [`Self::protocol`] reports the ALPN negotiated by the QUIC
	/// handshake, read from `conn` on demand — a connection that is still handshaking
	/// (from `into_0rtt`, say) is fine, it just has no ALPN to report until it is done.
	pub fn raw(conn: crate::quic::driver::Connection) -> Self {
		Self {
			conn,
			session_id: None,
			header_uni: Default::default(),
			header_bi: Default::default(),
			header_datagram: Default::default(),
			accept: None,
			settings: None,
			connect_send: Arc::new(Mutex::new(None)),
			close: Arc::new(CloseReason::new(true)),
			request: None,
			response: None,
			alpn: Default::default(),
		}
	}

	/// Returns the [`ConnectRequest`] if this session was established over HTTP/3,
	/// or `None` for a raw QUIC session.
	pub fn request(&self) -> Option<&ConnectRequest> {
		self.request.as_ref()
	}

	/// Returns the [`ConnectResponse`] if this session was established over HTTP/3,
	/// or `None` for a raw QUIC session.
	pub fn response(&self) -> Option<&ConnectResponse> {
		self.response.as_ref()
	}

	/// Returns the application protocol negotiated for this session.
	///
	/// For an HTTP/3 session this is the subprotocol the server selected via
	/// `WT-Available-Protocols`; for a raw QUIC session it is the negotiated ALPN.
	/// `None` if neither was negotiated, the ALPN is not valid UTF-8, or the raw
	/// connection is still handshaking.
	pub fn protocol(&self) -> Option<&str> {
		if let Some(response) = &self.response {
			return response.protocol.as_deref();
		}

		if let Some(alpn) = self.alpn.get() {
			return Some(alpn);
		}

		// Latch only a successful read, so a call made while the connection is still
		// handshaking cannot pin `None` for the life of the session.
		let alpn = negotiated_alpn(&self.conn)?;
		Some(self.alpn.get_or_init(|| alpn))
	}

	/// Return connection-level statistics.
	pub fn stats(&self) -> SessionStats {
		SessionStats(self.conn.stats())
	}
}

impl Deref for Session {
	type Target = crate::quic::driver::Connection;

	fn deref(&self) -> &Self::Target {
		&self.conn
	}
}

impl fmt::Debug for Session {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.conn.fmt(f)
	}
}

impl PartialEq for Session {
	fn eq(&self, other: &Self) -> bool {
		self.conn.stable_id() == other.conn.stable_id()
	}
}

impl Eq for Session {}

// Poll the shared accept state, then wake the *other* accepters once the lock is
// released.
//
// `SessionAccept` does not wake them itself: a waker is free to resume its task
// inline, and the first thing a resumed accepter does is take this same lock. An
// arrival or a failure is exactly what the others are parked waiting to retry
// after, so `Ready` is the signal.
fn poll_accept_uni_shared(accept: &Mutex<SessionAccept>, waiter: &Waiter) -> Poll<Result<RecvStream, SessionError>> {
	let (result, waiters, hold) = {
		let mut accept = accept.lock().unwrap();
		let waiters = accept.uni_waiters.clone();

		// The poll below drives the shared accept futures with this list's waker, and one
		// of them may wake it inline. `Fan` holds those back while this guard is alive.
		let hold = waiters.hold();
		let result = accept.poll_accept_uni(waiter);

		(result, waiters, hold)
	};

	// Dropped after the accept lock, never before: that is where a held-back wake is
	// delivered, and delivering it under the lock is the hazard the hold exists for.
	drop(hold);

	if result.is_ready() {
		waiters.wake();
	}

	result
}

fn poll_accept_bi_shared(
	accept: &Mutex<SessionAccept>,
	waiter: &Waiter,
) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
	let (result, waiters, hold) = {
		let mut accept = accept.lock().unwrap();
		let waiters = accept.bi_waiters.clone();

		// The poll below drives the shared accept futures with this list's waker, and one
		// of them may wake it inline. `Fan` holds those back while this guard is alive.
		let hold = waiters.hold();
		let result = accept.poll_accept_bi(waiter);

		(result, waiters, hold)
	};

	// Dropped after the accept lock, never before: that is where a held-back wake is
	// delivered, and delivering it under the lock is the hazard the hold exists for.
	drop(hold);

	if result.is_ready() {
		waiters.wake();
	}

	result
}

// Type aliases just so clippy doesn't complain about the complexity.
type AcceptUni =
	dyn Stream<Item = Result<crate::quic::driver::RecvStream, crate::quic::driver::ConnectionError>> + Send;
type AcceptBi = dyn Stream<
		Item = Result<
			(crate::quic::driver::SendStream, crate::quic::driver::RecvStream),
			crate::quic::driver::ConnectionError,
		>,
	> + Send;
type PendingUni = dyn Future<Output = Result<(StreamUni, crate::quic::driver::RecvStream), SessionError>> + Send;
type PendingBi = dyn Future<Output = Result<Option<(crate::quic::driver::SendStream, crate::quic::driver::RecvStream)>, SessionError>>
	+ Send;

// Logic just for accepting streams, which is annoying because of the stream header.
pub struct SessionAccept {
	session_id: VarInt,

	// Shared close reason for propagation to accepted streams.
	close: Arc<CloseReason>,

	// We also need to keep a reference to the qpack streams if the endpoint (incorrectly) creates
	// them. Again, this is just so they don't get closed until we drop the session.
	qpack_encoder: Option<crate::quic::driver::RecvStream>,
	qpack_decoder: Option<crate::quic::driver::RecvStream>,

	accept_uni: Pin<Box<AcceptUni>>,
	accept_bi: Pin<Box<AcceptBi>>,

	// Keep track of work being done to read/write the WebTransport stream header.
	pending_uni: FuturesUnordered<Pin<Box<PendingUni>>>,
	pending_bi: FuturesUnordered<Pin<Box<PendingBi>>>,

	// Waiters from concurrent callers of accept_bi / accept_uni.
	// Every clone of the session polls this one struct, so an arrival has to be fanned
	// out: each caller registers here and all of them are woken when a stream lands —
	// by the caller that saw it, once it has released the lock on this struct.
	bi_waiters: Fan,
	uni_waiters: Fan,

	// `Waker::from(waiters.clone())`, cached so the inner accept futures are polled with
	// the same waker every time. That waker outlives every caller, so an accepter that
	// drops its future cannot take the wakeup path with it, and the layer below holds
	// one registration rather than one per caller.
	bi_waker: Waker,
	uni_waker: Waker,
}

impl SessionAccept {
	pub(crate) fn new(conn: crate::quic::driver::Connection, session_id: VarInt, close: Arc<CloseReason>) -> Self {
		// Create a stream that just outputs new streams, so it's easy to call from poll.
		let accept_uni = Box::pin(futures::stream::unfold(conn.clone(), |conn| async {
			Some((conn.accept_uni().await, conn))
		}));

		let accept_bi = Box::pin(futures::stream::unfold(conn, |conn| async {
			Some((conn.accept_bi().await, conn))
		}));

		let bi_waiters = Fan::default();
		let uni_waiters = Fan::default();
		let bi_waker = bi_waiters.waker();
		let uni_waker = uni_waiters.waker();

		Self {
			session_id,
			close,

			qpack_decoder: None,
			qpack_encoder: None,

			accept_uni,
			accept_bi,

			pending_uni: FuturesUnordered::new(),
			pending_bi: FuturesUnordered::new(),

			bi_waiters,
			uni_waiters,
			bi_waker,
			uni_waker,
		}
	}

	/// Poll for the next unidirectional WebTransport stream.
	///
	/// `waiter` is parked until a stream arrives, the accept fails, or the caller drops
	/// it. The registration is weak and owned by the caller: keep the [`Waiter`] alive
	/// until it is woken, or it will be reclaimed and nothing will wake you. Drive this
	/// with [`kio::wait`], which holds the waiter inside the future it builds.
	///
	/// A `Ready` here means every *other* parked accepter should be woken so it can
	/// retry. This does not do that itself — see `poll_accept_uni_shared`, which wakes
	/// them once the lock on this struct is released.
	//
	// Poll-based because we accept and decode streams in parallel. In async land this
	// would be a `tokio::JoinSet`, but that needs a runtime; `FuturesUnordered` is
	// runtime-agnostic.
	pub fn poll_accept_uni(&mut self, waiter: &Waiter) -> Poll<Result<RecvStream, SessionError>> {
		// Register before polling, not on the way out: the shared waker can fire from the
		// layer below at any point here, and a wake that lands before the caller is on
		// the list would be lost.
		self.uni_waiters.register(waiter);

		let waker = self.uni_waker.clone();
		let cx = &mut Context::from_waker(&waker);

		loop {
			// Accept any new streams.
			if let Poll::Ready(Some(res)) = self.accept_uni.poll_next_unpin(cx) {
				// Start decoding the header and add the future to the list of pending streams.
				let recv = match res {
					Ok(recv) => recv,
					Err(e) => {
						return Poll::Ready(Err(e.into()));
					}
				};
				let pending = Self::decode_uni(recv, self.session_id);
				self.pending_uni.push(Box::pin(pending));

				continue;
			}

			// Poll the list of pending streams.
			let (typ, recv) = match self.pending_uni.poll_next_unpin(cx) {
				Poll::Ready(Some(Ok(res))) => res,
				Poll::Ready(Some(Err(err))) => {
					log_header_error(err, "unidirectional");
					continue;
				}
				Poll::Ready(None) | Poll::Pending => return Poll::Pending,
			};

			// Decide if we keep looping based on the type.
			match typ {
				StreamUni::WEBTRANSPORT => {
					let recv = RecvStream::new(recv, self.close.clone());
					return Poll::Ready(Ok(recv));
				}
				StreamUni::QPACK_DECODER => {
					self.qpack_decoder = Some(recv);
				}
				StreamUni::QPACK_ENCODER => {
					self.qpack_encoder = Some(recv);
				}
				_ => {
					// ignore unknown streams
					tracing::debug!(?typ, "ignoring unknown unidirectional stream");
				}
			}
		}
	}

	// Reads the stream header, returning the stream type.
	async fn decode_uni(
		mut recv: crate::quic::driver::RecvStream,
		expected_session: VarInt,
	) -> Result<(StreamUni, crate::quic::driver::RecvStream), SessionError> {
		// Read the VarInt at the start of the stream.
		let typ = StreamUni(read_varint(&mut recv).await?);

		if typ == StreamUni::WEBTRANSPORT {
			// Read the session_id and validate it
			let session_id = read_varint(&mut recv).await?;
			if session_id != expected_session {
				return Err(WebTransportError::UnknownSession.into());
			}
		}

		// We need to keep a reference to the qpack streams if the endpoint (incorrectly) creates
		// them, so return everything.
		Ok((typ, recv))
	}

	/// Poll for the next bidirectional WebTransport stream.
	///
	/// The same contract as [`poll_accept_uni`](Self::poll_accept_uni): the `waiter`
	/// registration is weak and owned by the caller, and a `Ready` is what the other
	/// parked accepters need to be woken for.
	pub fn poll_accept_bi(&mut self, waiter: &Waiter) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
		// Register before polling; see `poll_accept_uni`.
		self.bi_waiters.register(waiter);

		let waker = self.bi_waker.clone();
		let cx = &mut Context::from_waker(&waker);

		loop {
			// Accept any new streams.
			if let Poll::Ready(Some(res)) = self.accept_bi.poll_next_unpin(cx) {
				// Start decoding the header and add the future to the list of pending streams.
				let (send, recv) = match res {
					Ok(pair) => pair,
					Err(e) => {
						return Poll::Ready(Err(e.into()));
					}
				};
				let pending = Self::decode_bi(send, recv, self.session_id);
				self.pending_bi.push(Box::pin(pending));

				continue;
			}

			// Poll the list of pending streams.
			let res = match self.pending_bi.poll_next_unpin(cx) {
				Poll::Ready(Some(Ok(res))) => res,
				Poll::Ready(Some(Err(err))) => {
					log_header_error(err, "bidirectional");
					continue;
				}
				Poll::Ready(None) | Poll::Pending => return Poll::Pending,
			};

			if let Some((send, recv)) = res {
				// Wrap the streams in our own types for correct error codes.
				// The peer opened it, so this half carries no header.
				let send = SendStream::new(send, self.close.clone(), None);
				let recv = RecvStream::new(recv, self.close.clone());
				return Poll::Ready(Ok((send, recv)));
			}

			// Keep looping if it's a stream we want to ignore.
		}
	}

	// Reads the stream header, returning Some if it's a WebTransport stream.
	async fn decode_bi(
		send: crate::quic::driver::SendStream,
		mut recv: crate::quic::driver::RecvStream,
		expected_session: VarInt,
	) -> Result<Option<(crate::quic::driver::SendStream, crate::quic::driver::RecvStream)>, SessionError> {
		let typ = read_varint(&mut recv).await?;
		if Frame(typ) != Frame::WEBTRANSPORT {
			tracing::debug!(?typ, "ignoring unknown bidirectional stream");
			return Ok(None);
		}

		// Read the session ID and validate it.
		let session_id = read_varint(&mut recv).await?;
		if session_id != expected_session {
			return Err(WebTransportError::UnknownSession.into());
		}

		Ok(Some((send, recv)))
	}
}

// Read a stream header VarInt, keeping the read's real cause. `VarInt::read` reports any
// failure as a truncation, which would hide a stream the peer reset before its header
// arrived.
async fn read_varint(recv: &mut crate::quic::driver::RecvStream) -> Result<VarInt, WebTransportError> {
	let mut buf = [0u8; 8];
	recv.read_exact(&mut buf[..1]).await?;

	// The first two bits encode the length.
	let size = 1 << (buf[0] >> 6);
	recv.read_exact(&mut buf[1..size]).await?;

	Ok(VarInt::decode(&mut &buf[..size]).expect("a complete varint"))
}

// Without reliable reset, a stream the peer resets early loses its header with it, which
// moq does routinely. Anything else is a misbehaving peer.
fn log_header_error(err: SessionError, direction: &'static str) {
	match err {
		SessionError::WebTransportError(WebTransportError::ReadError(
			crate::quic::driver::ReadExactError::ReadError(
				crate::quic::driver::ReadError::Reset(_) | crate::quic::driver::ReadError::ConnectionLost(_),
			),
		)) => tracing::debug!(?err, direction, "stream closed before its header"),
		_ => tracing::warn!(?err, direction, "failed to decode stream header"),
	}
}

pub struct SessionStats(crate::quic::driver::ConnectionStats);

impl web_transport_trait::Stats for SessionStats {
	fn bytes_sent(&self) -> Option<u64> {
		Some(self.0.udp_tx.bytes)
	}

	fn bytes_received(&self) -> Option<u64> {
		Some(self.0.udp_rx.bytes)
	}

	fn bytes_lost(&self) -> Option<u64> {
		Some(self.0.path.lost_bytes)
	}

	fn packets_sent(&self) -> Option<u64> {
		Some(self.0.udp_tx.datagrams)
	}

	fn packets_received(&self) -> Option<u64> {
		Some(self.0.udp_rx.datagrams)
	}

	fn packets_lost(&self) -> Option<u64> {
		Some(self.0.path.lost_packets)
	}

	fn rtt(&self) -> Option<std::time::Duration> {
		Some(self.0.path.rtt)
	}

	/// The congestion controller's bandwidth estimate, in bits per second.
	fn estimated_send_rate(&self) -> Option<u64> {
		self.0.path.bandwidth_estimate.map(|bytes| bytes.saturating_mul(8))
	}
}

impl web_transport_trait::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = SessionError;

	async fn accept_uni(&self) -> Result<Self::RecvStream, Self::Error> {
		Self::accept_uni(self).await
	}

	async fn accept_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		Self::accept_bi(self).await
	}

	async fn open_bi(&self) -> Result<(Self::SendStream, Self::RecvStream), Self::Error> {
		Self::open_bi(self).await
	}

	async fn open_uni(&self) -> Result<Self::SendStream, Self::Error> {
		Self::open_uni(self).await
	}

	fn close(&self, code: u32, reason: &str) {
		Self::close(self, code, reason.as_bytes());
	}

	async fn closed(&self) -> Self::Error {
		Self::closed(self).await
	}

	fn send_datagram(&self, data: Bytes) -> Result<(), Self::Error> {
		Self::send_datagram(self, data)
	}

	async fn recv_datagram(&self) -> Result<Bytes, Self::Error> {
		Self::read_datagram(self).await
	}

	fn max_datagram_size(&self) -> usize {
		Self::max_datagram_size(self)
	}

	fn protocol(&self) -> Option<&str> {
		Self::protocol(self)
	}

	#[allow(refining_impl_trait)]
	fn stats(&self) -> SessionStats {
		Self::stats(self)
	}
}

#[cfg(all(test, any(feature = "aws-lc-rs", feature = "ring")))]
mod tests {
	use std::{
		io::{self, IoSliceMut},
		net::{Ipv4Addr, SocketAddr},
		sync::{
			Arc,
			atomic::{AtomicBool, Ordering},
		},
	};

	use rcgen::{CertifiedKey, KeyPair};
	use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

	use super::*;

	const SESSION: VarInt = VarInt::from_u32(0);

	#[derive(Debug, Default)]
	struct Gate {
		blocked: AtomicBool,
		attempted: tokio::sync::Notify,
		wakers: Mutex<Vec<Waker>>,
	}

	#[derive(Debug)]
	struct Socket {
		inner: Box<dyn crate::quic::driver::AsyncUdpSocket>,
		gate: Arc<Gate>,
	}

	#[derive(Debug)]
	struct Sender {
		inner: Pin<Box<dyn crate::quic::driver::UdpSender>>,
		gate: Arc<Gate>,
	}

	impl crate::quic::driver::UdpSender for Sender {
		fn poll_send(
			self: Pin<&mut Self>,
			transmit: &moq_sock::udp::Transmit<'_>,
			cx: &mut Context<'_>,
		) -> Poll<io::Result<()>> {
			let this = self.get_mut();
			let mut wakers = this.gate.wakers.lock().unwrap();
			if this.gate.blocked.load(Ordering::SeqCst) {
				if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
					wakers.push(cx.waker().clone());
				}
				this.gate.attempted.notify_one();
				return Poll::Pending;
			}
			drop(wakers);
			this.inner.as_mut().poll_send(transmit, cx)
		}
	}

	impl crate::quic::driver::AsyncUdpSocket for Socket {
		fn create_sender(&self) -> Pin<Box<dyn crate::quic::driver::UdpSender>> {
			Box::pin(Sender {
				inner: self.inner.create_sender(),
				gate: self.gate.clone(),
			})
		}
		fn poll_recv(
			&mut self,
			cx: &mut Context<'_>,
			bufs: &mut [IoSliceMut<'_>],
			meta: &mut [moq_sock::udp::RecvMeta],
		) -> Poll<io::Result<usize>> {
			self.inner.poll_recv(cx, bufs, meta)
		}
		fn local_addr(&self) -> io::Result<SocketAddr> {
			self.inner.local_addr()
		}
	}

	/// A connected client and server QUIC connection, plus the endpoints that drive them.
	async fn pair() -> (
		crate::quic::driver::Connection,
		crate::quic::driver::Connection,
		[crate::quic::driver::Endpoint; 2],
	) {
		let (client, server, endpoints, _) = gated_pair().await;
		(client, server, endpoints)
	}

	async fn gated_pair() -> (
		crate::quic::driver::Connection,
		crate::quic::driver::Connection,
		[crate::quic::driver::Endpoint; 2],
		Arc<Gate>,
	) {
		#[cfg(all(feature = "aws-lc-rs", feature = "ring"))]
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
		let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(&signing_key)));

		let server_config = crate::quic::driver::ServerConfig::with_single_cert(vec![cert.der().clone()], key).unwrap();
		let udp = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
		let gate = Arc::new(Gate::default());
		let socket = Socket {
			inner: crate::quic::driver::wrap_udp_socket(udp).unwrap(),
			gate: gate.clone(),
		};
		let server = crate::quic::driver::Endpoint::new_with_abstract_socket(
			Default::default(),
			Some(server_config),
			Box::new(socket),
		)
		.unwrap();

		let mut roots = rustls::RootCertStore::empty();
		roots.add(cert.der().clone()).unwrap();
		let client_config = crate::quic::driver::ClientConfig::with_root_certificates(Arc::new(roots)).unwrap();
		let client = crate::quic::driver::Endpoint::client((Ipv4Addr::LOCALHOST, 0).into()).unwrap();

		let connecting = client
			.connect_with(client_config, server.local_addr().unwrap(), "localhost")
			.unwrap();
		let accept = async { server.accept().await.unwrap().await.unwrap() };
		let (client_conn, server_conn) = tokio::join!(async { connecting.await.unwrap() }, accept);

		(client_conn, server_conn, [client, server], gate)
	}

	/// A queued capsule must survive a stalled socket beyond the teardown grace.
	#[tokio::test]
	async fn close_waits_for_capsule_ack() {
		close_with_stalled_socket(false).await;
	}

	/// Endpoint/drain cancellation must release a task waiting for a capsule ACK.
	#[tokio::test]
	async fn close_ack_wait_ends_with_connection() {
		close_with_stalled_socket(true).await;
	}

	async fn close_with_stalled_socket(cancel: bool) {
		let (client_conn, server_conn, _endpoints, gate) = gated_pair().await;
		let (client, server) = tokio::join!(
			Session::connect(client_conn, url::Url::parse("https://localhost/").unwrap()),
			async { crate::quic::web::Request::accept(server_conn).await.unwrap().ok().await },
		);
		let client = client.unwrap();
		let server = server.unwrap();
		let conn = server.conn.clone();
		let settings = Arc::downgrade(server.settings.as_ref().unwrap());
		tokio::time::pause();
		gate.blocked.store(true, Ordering::SeqCst);
		server.close(2, b"unauthorized");
		drop(server);
		gate.attempted.notified().await;
		tokio::time::advance(Duration::from_secs(1)).await;
		tokio::task::yield_now().await;
		assert!(
			conn.close_reason().is_none(),
			"closed before the capsule was acknowledged"
		);

		if cancel {
			conn.close(0u32.into(), b"drain deadline");
			conn.closed().await;
			tokio::task::yield_now().await;
			assert_eq!(
				settings.strong_count(),
				0,
				"close task retained the HTTP/3 streams after cancellation"
			);
			return;
		}

		gate.blocked.store(false, Ordering::SeqCst);
		for waker in std::mem::take(&mut *gate.wakers.lock().unwrap()) {
			waker.wake();
		}
		tokio::time::resume();
		let err = client.closed().await;
		assert!(
			matches!(err, SessionError::WebTransportError(WebTransportError::Closed(2, ref reason)) if reason == "unauthorized"),
			"{err:?}"
		);
		conn.closed().await;
	}

	fn assert_reset(err: SessionError) {
		assert!(
			matches!(
				&err,
				SessionError::WebTransportError(WebTransportError::ReadError(
					crate::quic::driver::ReadExactError::ReadError(crate::quic::driver::ReadError::Reset(code))
				)) if code.into_inner() == 7
			),
			"{err:?}"
		);
	}

	/// A CONNECT stream cut off by the connection's close is ordinary teardown, not a warning.
	#[tokio::test]
	#[tracing_test::traced_test]
	async fn capsule_read_ends_quietly_with_connection() {
		let (client, server, _endpoints) = pair().await;

		// A zero-length reserved frame, so the server sees the stream without a capsule.
		let (mut send, _recv) = client.open_bi().await.unwrap();
		send.write_all(&[0x21, 0x00]).await.unwrap();
		let (_send, recv) = server.accept_bi().await.unwrap();

		client.close(0u32.into(), b"done");
		assert!(Session::read_capsules(&server, recv).await.is_none());
		assert!(!logs_contain("failed to read capsule"), "an ordinary close warned");
	}

	/// A CONNECT stream that ends mid-frame on a live connection still warns.
	#[tokio::test]
	#[tracing_test::traced_test]
	async fn capsule_read_truncated_warns() {
		let (client, server, _endpoints) = pair().await;

		// A DATA frame that promises 5 bytes and carries 1.
		let (mut send, _recv) = client.open_bi().await.unwrap();
		send.write_all(&[0x00, 0x05, 0x01]).await.unwrap();
		send.finish().unwrap();
		let (_send, recv) = server.accept_bi().await.unwrap();

		assert!(Session::read_capsules(&server, recv).await.is_none());
		assert!(
			logs_contain("failed to read capsule"),
			"a truncated capsule stream was not reported"
		);
	}

	/// A uni stream reset before its header arrived reports the reset, not a foreign session.
	#[tokio::test]
	async fn uni_reset_before_header() {
		let (client, server, _endpoints) = pair().await;

		// The stream type alone, so the reset also cuts off the session ID.
		let mut send = client.open_uni().await.unwrap();
		send.write_all(&[0x40, 0x54]).await.unwrap();
		send.reset(7u32.into()).unwrap();

		let recv = server.accept_uni().await.unwrap();
		let err = SessionAccept::decode_uni(recv, SESSION).await.err().unwrap();
		assert_reset(err);
	}

	/// A bi stream reset before its header arrived reports the reset, not a foreign session.
	#[tokio::test]
	async fn bi_reset_before_header() {
		let (client, server, _endpoints) = pair().await;

		let (mut send, _recv) = client.open_bi().await.unwrap();
		send.reset(7u32.into()).unwrap();

		let (send, recv) = server.accept_bi().await.unwrap();
		let err = SessionAccept::decode_bi(send, recv, SESSION).await.err().unwrap();
		assert_reset(err);
	}

	/// A stream reset before any payload still delivers its header, so the peer attributes the
	/// reset to the session instead of dropping a stream it cannot place.
	#[tokio::test]
	async fn reset_delivers_header() {
		let (client_conn, server_conn, _endpoints) = pair().await;
		let (client, server) = tokio::join!(
			Session::connect(client_conn, url::Url::parse("https://localhost/").unwrap()),
			async { crate::quic::web::Request::accept(server_conn).await.unwrap().ok().await },
		);
		let (client, server) = (client.unwrap(), server.unwrap());

		client.open_uni().await.unwrap().reset(7).unwrap();
		let (mut send, _recv) = client.open_bi().await.unwrap();
		send.reset(8).unwrap();

		let read = async {
			let mut uni = server.accept_uni().await.unwrap();
			let (_send, mut bi) = server.accept_bi().await.unwrap();
			(
				uni.read_chunk(usize::MAX).await.unwrap_err(),
				bi.read_chunk(usize::MAX).await.unwrap_err(),
			)
		};
		// Without the header the server never surfaces the streams, so bound the wait.
		let (uni, bi) = tokio::time::timeout(Duration::from_secs(10), read).await.unwrap();
		assert!(matches!(uni, crate::quic::web::ReadError::Reset(7)), "{uni:?}");
		assert!(matches!(bi, crate::quic::web::ReadError::Reset(8)), "{bi:?}");
	}

	/// A stream that names another session is still reported as `UnknownSession`.
	#[tokio::test]
	async fn uni_other_session() {
		let (client, server, _endpoints) = pair().await;

		let mut send = client.open_uni().await.unwrap();
		send.write_all(&[0x40, 0x54, 0x04]).await.unwrap();
		send.finish().unwrap();

		let recv = server.accept_uni().await.unwrap();
		let err = SessionAccept::decode_uni(recv, SESSION).await.err().unwrap();
		assert!(
			matches!(err, SessionError::WebTransportError(WebTransportError::UnknownSession)),
			"{err:?}"
		);
	}
}
