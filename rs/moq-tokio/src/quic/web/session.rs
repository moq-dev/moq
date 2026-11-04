use std::{
	fmt,
	future::Future,
	io::Cursor,
	pin::Pin,
	sync::{Arc, Mutex},
	task::{Context, Poll, Waker},
	time::Duration,
};

use bytes::{Buf, Bytes};
use futures::stream::{FuturesUnordered, StreamExt};
use kio::Waiter;

use crate::quic::driver::send;
use crate::quic::web::fan::Fan;
use crate::quic::web::{
	ClientError, CloseReason, Connected, SessionError, Settings, WebTransportError,
	proto::{ConnectRequest, ConnectResponse, Frame, StreamUni, VarInt},
	recv::Stream as RecvStream,
	send::Stream as SendStream,
};

/// An established WebTransport session, acting like a full QUIC connection. See
/// [`crate::quic::driver::connection::Connection`].
///
/// It is important to remember that WebTransport is layered on top of QUIC:
///   1. Each stream starts with a few bytes identifying the stream type and session ID.
///   2. Errors codes are encoded with the session ID, so they aren't full QUIC error codes.
///   3. Stream IDs may have gaps in them, used by HTTP/3 transparent to the application.
///
#[derive(Clone)]
pub struct Session {
	conn: crate::quic::driver::connection::Connection,

	// The session ID, as determined by the stream ID of the connect request.
	session_id: Option<VarInt>,

	// The accept logic is stateful, so use an Arc<Mutex> to share it.
	accept: Option<Arc<Mutex<SessionAccept>>>,

	// Cache the headers in front of each stream we open.
	header_uni: Bytes,
	header_bi: Bytes,
	header_datagram: Bytes,

	#[allow(
		dead_code,
		reason = "keep a reference to the settings and connect stream to avoid closing them until dropped"
	)]
	settings: Option<Arc<Settings>>,

	// The send side of the CONNECT stream, used to write the CloseWebTransportSession capsule.
	// Wrapped in Arc<Mutex<Option<...>>> so close() can take it exactly once.
	connect_send: Arc<Mutex<Option<crate::quic::driver::send::Stream>>>,

	// Why the session closed: set once by a local close() or the background task
	// when a remote CloseWebTransportSession capsule is received.
	close: Arc<CloseReason>,

	// The request sent by the client, or None for a raw QUIC session.
	request: Option<ConnectRequest>,

	// The response sent by the server, or None for a raw QUIC session.
	response: Option<ConnectResponse>,

	// The ALPN the QUIC handshake negotiated, for raw QUIC sessions, copied out of the
	// connection's lock so `protocol()` can borrow it. HTTP/3 sessions read the
	// subprotocol out of the response instead.
	alpn: Option<Arc<str>>,
	opening: Arc<kio::Bridge<Opening>>,
}

impl Session {
	pub(crate) fn new(
		conn: crate::quic::driver::connection::Connection,
		mut settings: Settings,
		connect: Connected,
	) -> Self {
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
		let early = std::mem::take(settings.early.get_mut().unwrap());
		let accept = SessionAccept::new(conn.clone(), session_id, close.clone(), early);

		let this = Self {
			conn,
			accept: Some(Arc::new(Mutex::new(accept))),
			session_id: Some(session_id),
			header_uni: header_uni.into(),
			header_bi: header_bi.into(),
			header_datagram: header_datagram.into(),
			settings: Some(Arc::new(settings)),
			connect_send: Arc::new(Mutex::new(Some(connect.send))),
			close: close.clone(),
			request: Some(connect.request.clone()),
			response: Some(connect.response.clone()),
			alpn: None,
			opening: Default::default(),
		};

		// Run a background task to read capsules from the CONNECT recv stream.
		let conn2 = this.conn.clone();
		crate::runtime::detach(crate::panic::isolate(Self::run_recv(conn2, connect.recv, close)));

		this
	}

	// Read capsules from the CONNECT recv stream until it's closed,
	// then record the close error and tear down the connection.
	async fn run_recv(
		conn: crate::quic::driver::connection::Connection,
		recv: crate::quic::driver::recv::Stream,
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
				conn.close(http3_code, &reason);
			}
			None => {
				let err = crate::quic::driver::connection::Error::LocallyClosed {
					code: http3_code,
					reason: String::new(),
				};
				if !close.set(err.into()) {
					return;
				}
				conn.close(http3_code, "");
			}
		};
	}

	// Keep reading capsules from the CONNECT recv stream until it's closed.
	// Returns Some((code, reason)) if a CloseWebTransportSession capsule was received,
	// or None if the stream closed without a capsule.
	async fn read_capsules(
		conn: &crate::quic::driver::connection::Connection,
		recv: crate::quic::driver::recv::Stream,
	) -> Option<(u32, String)> {
		let mut reader = web_transport_proto::Http3CapsuleReader::new(recv);
		loop {
			match reader.read().await {
				Ok(Some(web_transport_proto::Capsule::CloseWebTransportSession { code, reason })) => {
					return Some((code, reason));
				}
				Ok(Some(web_transport_proto::Capsule::Grease { .. })) => {}
				Ok(Some(web_transport_proto::Capsule::Unknown { typ, payload })) => {
					tracing::debug!(%typ, size = payload.len(), "unknown capsule");
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
		conn: crate::quic::driver::connection::Connection,
		request: impl Into<ConnectRequest>,
	) -> Result<Session, ClientError> {
		crate::runtime::scope(async move {
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
		})
		.await
	}

	/// Accept the next unidirectional stream after its WebTransport header.
	pub async fn accept_uni(&self) -> Result<RecvStream, SessionError> {
		kio::wait(|waiter| self.poll_accept_uni(waiter)).await
	}

	/// Accept the next bidirectional stream after its WebTransport header.
	pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), SessionError> {
		kio::wait(|waiter| self.poll_accept_bi(waiter)).await
	}

	/// Open a unidirectional stream scheduled by `options`.
	pub async fn open_uni(&self, options: send::Options<'_>) -> Result<SendStream, SessionError> {
		kio::wait(|waiter| self.poll_open_uni(waiter, options)).await
	}

	/// Open a bidirectional stream scheduled by `options`.
	pub async fn open_bi(&self, options: send::Options<'_>) -> Result<(SendStream, RecvStream), SessionError> {
		kio::wait(|waiter| self.poll_open_bi(waiter, options)).await
	}

	/// Poll for the next unidirectional stream after its WebTransport header.
	pub fn poll_accept_uni(&self, waiter: &Waiter) -> Poll<Result<RecvStream, SessionError>> {
		if let Some(accept) = &self.accept {
			poll_accept_uni_shared(accept, waiter).map_err(|e| self.map_error(e))
		} else {
			self.conn
				.poll_accept_uni(waiter)
				.map_ok(|recv| RecvStream::new(recv, self.close.clone()))
				.map_err(|e| self.map_error(e))
		}
	}

	/// Poll for the next bidirectional stream after its WebTransport header.
	pub fn poll_accept_bi(&self, waiter: &Waiter) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
		if let Some(accept) = &self.accept {
			poll_accept_bi_shared(accept, waiter).map_err(|e| self.map_error(e))
		} else {
			self.conn
				.poll_accept_bi(waiter)
				.map_ok(|(send, recv)| {
					(
						SendStream::new(send, self.close.clone(), None),
						RecvStream::new(recv, self.close.clone()),
					)
				})
				.map_err(|e| self.map_error(e))
		}
	}

	/// Poll to open a unidirectional stream and write its WebTransport header.
	pub fn poll_open_uni(&self, waiter: &Waiter, options: send::Options<'_>) -> Poll<Result<SendStream, SessionError>> {
		self.opening.poll(waiter, |opening, cx| {
			if opening.uni.is_none() {
				let send = std::task::ready!(self.conn.poll_open_uni(
					opening.park.hold(cx),
					send::Options {
						group: None,
						priority: i64::MAX
					}
				))
				.map_err(|e| self.map_error(e))?;
				opening.uni = Some((send, self.header_uni.clone()));
			}
			let (send, header) = opening.uni.as_mut().unwrap();
			let result = std::task::ready!(poll_header(send, opening.park.hold(cx), header));
			let (mut send, _) = opening.uni.take().unwrap();
			result.map_err(|e| self.map_error(e))?;
			send.set_priority(options.priority);
			send.set_group(options.group);
			let header = self.session_id.map(|_| send.offset());
			Poll::Ready(Ok(SendStream::new(send, self.close.clone(), header)))
		})
	}

	/// Poll to open a bidirectional stream and write its WebTransport header.
	pub fn poll_open_bi(
		&self,
		waiter: &Waiter,
		options: send::Options<'_>,
	) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
		self.opening.poll(waiter, |opening, cx| {
			if opening.bi.is_none() {
				let (mut send, recv) = std::task::ready!(self.conn.poll_open_bi(
					opening.park.hold(cx),
					send::Options {
						group: None,
						priority: i64::MAX
					}
				))
				.map_err(|e| self.map_error(e))?;
				send.set_priority(i64::MAX);
				opening.bi = Some((send, recv, self.header_bi.clone()));
			}
			let (send, _, header) = opening.bi.as_mut().unwrap();
			let result = std::task::ready!(poll_header(send, opening.park.hold(cx), header));
			let (mut send, recv, _) = opening.bi.take().unwrap();
			result.map_err(|e| self.map_error(e))?;
			send.set_priority(options.priority);
			send.set_group(options.group);
			let header = self.session_id.map(|_| send.offset());
			Poll::Ready(Ok((
				SendStream::new(send, self.close.clone(), header),
				RecvStream::new(recv, self.close.clone()),
			)))
		})
	}

	/// Receive the next datagram with its WebTransport session header removed.
	pub async fn recv_datagram(&self) -> Result<Bytes, SessionError> {
		kio::wait(|waiter| self.poll_recv_datagram(waiter)).await
	}

	/// Poll for a datagram with its WebTransport session header removed.
	///
	/// A datagram that does not name this session, such as another session's on the same
	/// connection, is dropped: datagrams are unreliable, so a stray one must not end the session.
	pub fn poll_recv_datagram(&self, waiter: &Waiter) -> Poll<Result<Bytes, SessionError>> {
		loop {
			let mut datagram =
				std::task::ready!(self.conn.poll_recv_datagram(waiter)).map_err(|e| self.map_error(e))?;
			let Some(session_id) = self.session_id else {
				return Poll::Ready(Ok(datagram));
			};
			let mut cursor = Cursor::new(&datagram);
			match VarInt::decode(&mut cursor) {
				Ok(actual) if actual == session_id => {
					return Poll::Ready(Ok(datagram.split_off(cursor.position() as usize)));
				}
				_ => tracing::debug!("dropping a datagram for an unknown session"),
			}
		}
	}

	/// Sends an application datagram to the remote peer, scheduled by `options`.
	///
	/// Datagrams are unreliable and may be dropped or delivered out of order.
	/// The data must be smaller than [`max_datagram_size`](Self::max_datagram_size).
	/// See [`Connection::send_datagram`](crate::quic::driver::connection::Connection::send_datagram) for how
	/// `options` schedule it.
	pub fn send_datagram(&self, data: impl Buf, options: send::Options<'_>) -> Result<(), SessionError> {
		// Prepend the header naming the session, which is empty for a raw QUIC session. The
		// payload is chained rather than copied.
		let datagram = self.header_datagram.clone().chain(data);
		self.conn
			.send_datagram(datagram, options)
			.map_err(|e| self.map_error(e))?;
		Ok(())
	}

	/// Open a send group at `priority`, for streams and datagrams to join with [`send::Options`]
	/// or [`SendStream::set_group`].
	///
	/// See [`crate::quic::driver::send::Group`]. The HTTP/3 streams stay outside every group,
	/// so they go first.
	pub fn send_group(&self, priority: i64) -> crate::quic::driver::send::Group {
		self.conn.send_group(priority)
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
	pub fn close(&self, code: u32, reason: &str) {
		// First close wins: a no-op if the connection already closed, the background
		// task recorded a remote close, or close() was already called.
		if let Some(err) = self.conn.close_reason() {
			self.close.set(self.map_error(err));
			return;
		}
		let err = SessionError::ConnectionError(crate::quic::driver::connection::Error::LocallyClosed {
			code: code.into(),
			reason: reason.to_string(),
		});
		if !self.close.set(err) {
			return;
		}

		if self.session_id.is_some() {
			// Take the send stream for the capsule write.
			let send = self.connect_send.lock().unwrap().take();

			if let Some(send) = send {
				let capsule = web_transport_proto::Capsule::CloseWebTransportSession {
					code,
					reason: reason.to_string(),
				};
				let timeout = (self.conn.rtt() * 3).max(Duration::from_millis(100));

				// The task owns a clone of the whole session, not just the connection:
				// dropping the caller's last handle would otherwise end the HTTP/3
				// control and QPACK streams before the capsule is delivered, which a
				// browser treats as a connection error, losing the code and reason.
				let session = self.clone();
				crate::runtime::detach(crate::panic::isolate(async move {
					session.close_with_capsule(send, capsule, code, timeout).await;
				}));
			}
		} else {
			// Raw QUIC mode: no capsule needed.
			self.conn.close(code.into(), reason);
		}
	}

	/// Deliver the close capsule and FIN, then give the peer its teardown grace.
	async fn close_with_capsule(
		&self,
		mut send: crate::quic::driver::send::Stream,
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
			self.conn.close(http3_code, "");
			return;
		};
		len.encode(&mut frame);
		frame.extend_from_slice(&capsule_bytes);

		// Write the DATA frame to the CONNECT send stream.
		if let Err(e) = send.write_all(&frame[..]).await {
			tracing::warn!(?e, "failed to write CloseWebTransportSession capsule");
			self.conn.close(http3_code, "");
			return;
		}

		// FIN the send stream so the peer knows no more capsules are coming.
		let send = send.finish();

		// A queued write is not delivery: tearing down QUIC before the DATA and FIN
		// are acknowledged can discard the capsule during loss recovery. Transport
		// failure (including endpoint shutdown) also completes this wait.
		match send.await {
			Ok(None) => {}
			Ok(Some(code)) => {
				tracing::debug!(?code, "peer stopped the CONNECT stream during close");
				self.conn.close(http3_code, "");
				return;
			}
			Err(e) => {
				tracing::debug!(?e, "connection ended before the close capsule was acknowledged");
				return;
			}
		}

		// Wait for the peer to close the CONNECT stream after receiving the capsule.
		if kio::clock::timeout(timeout, self.conn.closed()).await.is_err() {
			tracing::debug!("timeout waiting for peer to close; force-closing connection");
			self.conn.close(http3_code, "");
		}
	}

	/// Wait until the session is closed, returning the error. See [`crate::quic::driver::connection::Connection::closed`].
	///
	/// If the peer sent a `CloseWebTransportSession` capsule, the returned error will be
	/// [`WebTransportError::Closed`] with the code and reason from the capsule.
	///
	/// Unlike [`crate::quic::driver::connection::Connection::closed`], this does **not** return early when
	/// [`close()`](Self::close) has been called. It waits for the underlying QUIC
	/// connection to shut down, ensuring the `CloseWebTransportSession` capsule has
	/// been delivered. Use [`close_reason()`](Self::close_reason) for a non-blocking check.
	pub async fn closed(&self) -> SessionError {
		kio::wait(|waiter| self.poll_closed(waiter)).await
	}

	/// Return why the session was closed, or None if it's not closed. See
	/// [`crate::quic::driver::connection::Connection::close_reason`].
	pub fn close_reason(&self) -> Option<SessionError> {
		self.conn.close_reason().map(|e| self.map_error(e))
	}

	/// Report an error caused by the connection closing as the session's close reason.
	fn map_error(&self, e: impl Into<SessionError>) -> SessionError {
		self.close.map(e.into())
	}

	/// Create a new session from a raw QUIC connection.
	///
	/// This is used to pretend like a QUIC connection is a WebTransport session,
	/// making it easier to support WebTransport and raw QUIC simultaneously.
	///
	/// There is no HTTP/3 exchange, so [`Self::request`] and [`Self::response`] both
	/// return `None`. [`Self::protocol`] reports the ALPN negotiated by the QUIC
	/// handshake, when one was negotiated.
	pub fn raw(conn: crate::quic::driver::connection::Connection) -> Self {
		// A connection exists only once its handshake completed, so the ALPN is settled.
		let alpn = conn
			.handshake_data()
			.and_then(|data| String::from_utf8(data.alpn?).ok())
			.map(Arc::from);
		Self {
			alpn,
			opening: Default::default(),
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
	/// `None` if neither was negotiated or the ALPN is not valid UTF-8.
	pub fn protocol(&self) -> Option<&str> {
		match &self.response {
			Some(response) => response.protocol.as_deref(),
			None => self.alpn.as_deref(),
		}
	}

	/// Poll until the session's connection has closed.
	pub fn poll_closed(&self, waiter: &Waiter) -> Poll<SessionError> {
		self.conn.poll_closed(waiter).map(|e| self.map_error(e))
	}

	/// The QUIC connection carrying this session.
	pub fn connection(&self) -> &crate::quic::driver::connection::Connection {
		&self.conn
	}

	/// Return connection-level statistics.
	pub fn stats(&self) -> crate::quic::driver::connection::Stats {
		self.conn.stats()
	}
}

#[derive(Default)]
struct Opening {
	// Driver wakes must go through the bridge, so they cannot reenter its held lock.
	park: kio::Park,
	uni: Option<(send::Stream, Bytes)>,
	bi: Option<(send::Stream, crate::quic::driver::recv::Stream, Bytes)>,
}

fn poll_header(send: &mut send::Stream, waiter: &Waiter, header: &mut Bytes) -> Poll<Result<(), SessionError>> {
	while header.has_remaining() {
		std::task::ready!(send.poll_write_buf(waiter, header)).map_err(|err| match err {
			crate::quic::driver::send::Error::Connection(err) => SessionError::from(err),
			err => WebTransportError::WriteError(err).into(),
		})?;
	}
	Poll::Ready(Ok(()))
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

/// A unidirectional stream's header: its type, the session a WebTransport stream names, and the
/// stream positioned after it.
pub(crate) type UniHeader = (StreamUni, Option<VarInt>, crate::quic::driver::recv::Stream);
/// A unidirectional stream whose header is being read.
pub(crate) type PendingUni = dyn Future<Output = Result<UniHeader, SessionError>> + Send;
type PendingBi = dyn Future<
		Output = Result<Option<(crate::quic::driver::send::Stream, crate::quic::driver::recv::Stream)>, SessionError>,
	> + Send;

// Logic just for accepting streams, which is annoying because of the stream header.
struct SessionAccept {
	session_id: VarInt,

	// Shared close reason for propagation to accepted streams.
	close: Arc<CloseReason>,

	// We also need to keep a reference to the qpack streams if the endpoint (incorrectly) creates
	// them. Again, this is just so they don't get closed until we drop the session.
	qpack_encoder: Option<crate::quic::driver::recv::Stream>,
	qpack_decoder: Option<crate::quic::driver::recv::Stream>,

	conn: crate::quic::driver::connection::Connection,
	uni_park: kio::Park,
	bi_park: kio::Park,
	// Pending WebTransport headers must be decoded before returning this error.
	uni_end: Option<SessionError>,
	bi_end: Option<SessionError>,

	// Keep track of work being done to read/write the WebTransport stream header.
	pending_uni: FuturesUnordered<Pin<Box<PendingUni>>>,
	pending_bi: FuturesUnordered<Pin<Box<PendingBi>>>,

	// Waiters from concurrent callers of accept_bi / accept_uni.
	// Every clone of the session polls this one struct, so an arrival has to be fanned
	// out: each caller registers here and all of them are woken when a stream lands -
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
	/// Accept `session_id`'s streams on `conn`, starting with the unidirectional streams the
	/// HTTP/3 handshake found ahead of the peer's control stream.
	pub(crate) fn new(
		conn: crate::quic::driver::connection::Connection,
		session_id: VarInt,
		close: Arc<CloseReason>,
		early: Vec<Pin<Box<PendingUni>>>,
	) -> Self {
		let bi_waiters = Fan::default();
		let uni_waiters = Fan::default();
		let bi_waker = bi_waiters.waker();
		let uni_waker = uni_waiters.waker();

		Self {
			session_id,
			close,

			qpack_decoder: None,
			qpack_encoder: None,

			conn,
			uni_park: Default::default(),
			bi_park: Default::default(),
			uni_end: None,
			bi_end: None,

			pending_uni: early.into_iter().collect(),
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
	/// retry. This does not do that itself - see `poll_accept_uni_shared`, which wakes
	/// them once the lock on this struct is released.
	//
	// Poll-based because we accept and decode streams in parallel. In async land this
	// would be a `tokio::JoinSet`, but that needs a runtime; `FuturesUnordered` is
	// runtime-agnostic.
	fn poll_accept_uni(&mut self, waiter: &Waiter) -> Poll<Result<RecvStream, SessionError>> {
		// Register before polling, not on the way out: the shared waker can fire from the
		// layer below at any point here, and a wake that lands before the caller is on
		// the list would be lost.
		self.uni_waiters.register(waiter);

		let waker = self.uni_waker.clone();
		let cx = &mut Context::from_waker(&waker);

		loop {
			// Accept any new streams.
			if self.uni_end.is_none()
				&& let Poll::Ready(res) = self.conn.poll_accept_uni(self.uni_park.hold(cx))
			{
				// Start decoding the header and add the future to the list of pending streams.
				match res {
					Ok(recv) => {
						self.pending_uni.push(Box::pin(read_uni_header(recv)));
						continue;
					}
					Err(err) => self.uni_end = Some(err.into()),
				}
			}

			// Poll the list of pending streams.
			let (typ, session, recv) = match self.pending_uni.poll_next_unpin(cx) {
				Poll::Ready(Some(Ok(res))) => res,
				Poll::Ready(Some(Err(err))) => {
					log_header_error(err, "unidirectional");
					continue;
				}
				Poll::Ready(None) => return self.uni_end.clone().map_or(Poll::Pending, |err| Poll::Ready(Err(err))),
				Poll::Pending => return Poll::Pending,
			};

			// Decide if we keep looping based on the type.
			match typ {
				StreamUni::WEBTRANSPORT if session == Some(self.session_id) => {
					let recv = RecvStream::new(recv, self.close.clone());
					return Poll::Ready(Ok(recv));
				}
				StreamUni::WEBTRANSPORT => {
					log_header_error(WebTransportError::UnknownSession.into(), "unidirectional");
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

	/// Poll for the next bidirectional WebTransport stream.
	///
	/// The same contract as [`poll_accept_uni`](Self::poll_accept_uni): the `waiter`
	/// registration is weak and owned by the caller, and a `Ready` is what the other
	/// parked accepters need to be woken for.
	fn poll_accept_bi(&mut self, waiter: &Waiter) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
		// Register before polling; see `poll_accept_uni`.
		self.bi_waiters.register(waiter);

		let waker = self.bi_waker.clone();
		let cx = &mut Context::from_waker(&waker);

		loop {
			// Accept any new streams.
			if self.bi_end.is_none()
				&& let Poll::Ready(res) = self.conn.poll_accept_bi(self.bi_park.hold(cx))
			{
				// Start decoding the header and add the future to the list of pending streams.
				match res {
					Ok((send, recv)) => {
						self.pending_bi
							.push(Box::pin(Self::decode_bi(send, recv, self.session_id)));
						continue;
					}
					Err(err) => self.bi_end = Some(err.into()),
				}
			}

			// Poll the list of pending streams.
			let res = match self.pending_bi.poll_next_unpin(cx) {
				Poll::Ready(Some(Ok(res))) => res,
				Poll::Ready(Some(Err(err))) => {
					log_header_error(err, "bidirectional");
					continue;
				}
				Poll::Ready(None) => return self.bi_end.clone().map_or(Poll::Pending, |err| Poll::Ready(Err(err))),
				Poll::Pending => return Poll::Pending,
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
		send: crate::quic::driver::send::Stream,
		mut recv: crate::quic::driver::recv::Stream,
		expected_session: VarInt,
	) -> Result<Option<(crate::quic::driver::send::Stream, crate::quic::driver::recv::Stream)>, SessionError> {
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

/// Read a unidirectional stream's header: its type, then the session ID if it is a WebTransport
/// stream. Which session is the caller's to check.
pub(crate) async fn read_uni_header(mut recv: crate::quic::driver::recv::Stream) -> Result<UniHeader, SessionError> {
	let typ = StreamUni(read_varint(&mut recv).await?);
	let session = match typ {
		StreamUni::WEBTRANSPORT => Some(read_varint(&mut recv).await?),
		_ => None,
	};
	Ok((typ, session, recv))
}

// Read a stream header VarInt, keeping the read's real cause. `VarInt::read` reports any
// failure as a truncation, which would hide a stream the peer reset before its header
// arrived.
async fn read_varint(recv: &mut crate::quic::driver::recv::Stream) -> Result<VarInt, WebTransportError> {
	let mut buf = [0u8; 8];
	recv.read_exact(&mut buf[..1]).await?;

	// The first two bits encode the length.
	let size = 1 << (buf[0] >> 6);
	recv.read_exact(&mut buf[1..size]).await?;

	Ok(VarInt::decode(&mut &buf[..size]).expect("a complete varint"))
}

// Without reliable reset, a stream the peer resets early loses its header with it, which
// moq does routinely. Anything else is a misbehaving peer.
pub(crate) fn log_header_error(err: SessionError, direction: &'static str) {
	match err {
		SessionError::WebTransportError(WebTransportError::ReadError(
			crate::quic::driver::recv::Error::Reset(_) | crate::quic::driver::recv::Error::Connection(_),
		)) => tracing::debug!(?err, direction, "stream closed before its header"),
		_ => tracing::warn!(?err, direction, "failed to decode stream header"),
	}
}

struct Metrics(crate::quic::driver::connection::Stats);

impl moq_net::transport::Metrics for Metrics {
	fn streams_evicted(&self) -> Option<u64> {
		Some(self.0.credit.evicted)
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

impl moq_net::transport::poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = SessionError;
	fn poll_accept_uni(&self, waiter: &Waiter) -> Poll<Result<RecvStream, SessionError>> {
		Self::poll_accept_uni(self, waiter)
	}
	fn poll_accept_bi(&self, waiter: &Waiter) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
		Self::poll_accept_bi(self, waiter)
	}
	fn poll_open_uni(
		&self,
		waiter: &Waiter,
		options: moq_net::transport::poll::SendOptions<'_, send::Group>,
	) -> Poll<Result<SendStream, SessionError>> {
		Self::poll_open_uni(
			self,
			waiter,
			send::Options {
				group: options.group,
				priority: options.priority,
			},
		)
	}
	fn poll_open_bi(
		&self,
		waiter: &Waiter,
		options: moq_net::transport::poll::SendOptions<'_, send::Group>,
	) -> Poll<Result<(SendStream, RecvStream), SessionError>> {
		Self::poll_open_bi(
			self,
			waiter,
			send::Options {
				group: options.group,
				priority: options.priority,
			},
		)
	}
	fn send_group(&self, priority: i64) -> Option<send::Group> {
		Some(Self::send_group(self, priority))
	}
	fn send_datagram(
		&self,
		payload: Bytes,
		options: moq_net::transport::poll::SendOptions<'_, send::Group>,
	) -> Result<(), SessionError> {
		Self::send_datagram(
			self,
			payload,
			send::Options {
				group: options.group,
				priority: options.priority,
			},
		)
	}
	fn poll_recv_datagram(&self, waiter: &Waiter) -> Poll<Result<Bytes, SessionError>> {
		Self::poll_recv_datagram(self, waiter)
	}
	fn max_datagram_size(&self) -> usize {
		Self::max_datagram_size(self)
	}
	fn protocol(&self) -> Option<&str> {
		Self::protocol(self)
	}
	fn address(&self) -> moq_net::transport::Address {
		if self.session_id.is_some() {
			moq_net::transport::Address::Uri
		} else {
			moq_net::transport::Address::Setup
		}
	}
	fn close(&self, code: u32, reason: &str) {
		Self::close(self, code, reason);
	}
	fn poll_closed(&self, waiter: &Waiter) -> Poll<SessionError> {
		Self::poll_closed(self, waiter)
	}
	fn metrics(&self) -> impl moq_net::transport::Metrics {
		Metrics(self.conn.stats())
	}
	fn wire(&self) -> Option<moq_net::transport::Wire> {
		Some(self.conn.wire())
	}
	fn probe(&self) -> Option<moq_net::transport::Probe> {
		self.conn.probe()
	}
	fn receive_window(&self) -> Option<moq_net::transport::ReceiveWindow> {
		Some(self.conn.receive_window())
	}
	/// The connection's credit, which every WebTransport session on it shares.
	fn stream_credit(&self, dir: moq_net::transport::Dir) -> Option<moq_net::transport::StreamCredit> {
		Some(self.conn.stream_credit(dir))
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
		inner: Box<dyn crate::quic::driver::endpoint::AsyncUdpSocket>,
		gate: Arc<Gate>,
	}

	impl crate::quic::driver::endpoint::AsyncUdpSocket for Socket {
		fn poll_recv(
			&self,
			cx: &mut Context<'_>,
			bufs: &mut [IoSliceMut<'_>],
			meta: &mut [moq_sock::udp::RecvMeta],
		) -> Poll<io::Result<usize>> {
			self.inner.poll_recv(cx, bufs, meta)
		}

		fn try_send(&self, transmit: &moq_sock::udp::Transmit<'_>) -> io::Result<()> {
			if self.gate.blocked.load(Ordering::SeqCst) {
				self.gate.attempted.notify_one();
				return Err(io::ErrorKind::WouldBlock.into());
			}
			self.inner.try_send(transmit)
		}

		fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
			let mut wakers = self.gate.wakers.lock().unwrap();
			if self.gate.blocked.load(Ordering::SeqCst) {
				if !wakers.iter().any(|waker| waker.will_wake(cx.waker())) {
					wakers.push(cx.waker().clone());
				}
				return Poll::Pending;
			}
			drop(wakers);
			self.inner.poll_writable(cx)
		}

		fn local_addr(&self) -> io::Result<SocketAddr> {
			self.inner.local_addr()
		}
	}

	/// A connected client and server QUIC connection, plus the endpoints that drive them.
	async fn pair() -> (
		crate::quic::driver::connection::Connection,
		crate::quic::driver::connection::Connection,
		[crate::quic::driver::endpoint::Endpoint; 2],
	) {
		let (client, server, endpoints, _) = gated_pair().await;
		(client, server, endpoints)
	}

	#[tokio::test]
	#[tracing_test::traced_test]
	async fn connect_credentials_stay_out_of_traces() {
		let (client, server, _endpoints) = pair().await;
		let url: url::Url = "https://localhost/room?jwt=url-credential".parse().unwrap();
		let request = ConnectRequest::new(url)
			.with_header(http::header::AUTHORIZATION, "Bearer header-credential".parse().unwrap())
			.with_header(http::header::COOKIE, "session=cookie-credential".parse().unwrap());
		let accept = async {
			let request = crate::quic::web::Connecting::accept(&server).await.unwrap();
			assert_eq!(request.headers[http::header::COOKIE], "session=cookie-credential");
			request.respond(http::StatusCode::OK).await.unwrap()
		};
		let (client, server) = tokio::join!(Connected::open(&client, request), accept);
		assert_eq!(client.unwrap().request.url, server.request.url);
		assert!(logs_contain("sending CONNECT request"));
		assert!(logs_contain("received CONNECT request"));
		for secret in ["url-credential", "header-credential", "cookie-credential"] {
			assert!(!logs_contain(secret));
		}
	}

	async fn gated_pair() -> (
		crate::quic::driver::connection::Connection,
		crate::quic::driver::connection::Connection,
		[crate::quic::driver::endpoint::Endpoint; 2],
		Arc<Gate>,
	) {
		gated_pair_with_transport(Default::default(), Default::default()).await
	}

	async fn gated_pair_with_transport(
		server_transport: crate::quic::driver::transport::Config,
		client_transport: crate::quic::driver::transport::Config,
	) -> (
		crate::quic::driver::connection::Connection,
		crate::quic::driver::connection::Connection,
		[crate::quic::driver::endpoint::Endpoint; 2],
		Arc<Gate>,
	) {
		#[cfg(all(feature = "aws-lc-rs", feature = "ring"))]
		let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

		let CertifiedKey { cert, signing_key } = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
		let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(KeyPair::serialize_der(&signing_key)));

		let mut server_config =
			crate::quic::driver::server::Config::with_single_cert(vec![cert.der().clone()], key).unwrap();
		server_config.transport = server_transport;
		let udp = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
		let gate = Arc::new(Gate::default());
		let socket = Socket {
			inner: crate::quic::driver::wrap_udp_socket(udp).unwrap(),
			gate: gate.clone(),
		};
		let socket: Box<dyn crate::quic::driver::endpoint::AsyncUdpSocket> = Box::new(socket);
		let server = crate::quic::driver::endpoint::Endpoint::new(
			crate::quic::driver::endpoint::Config::default().with_server(server_config),
			socket,
		)
		.unwrap();

		let mut roots = rustls::RootCertStore::empty();
		roots.add(cert.der().clone()).unwrap();
		let mut client_config = crate::quic::driver::client::Config::with_root_certificates(Arc::new(roots)).unwrap();
		client_config.transport = client_transport;
		let udp = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
		let client = crate::quic::driver::endpoint::Endpoint::new(Default::default(), udp).unwrap();

		let connecting = client
			.connect(&client_config, server.local_addr(), "localhost")
			.unwrap();
		let accept = async { server.accept().await.unwrap().accept().unwrap().await.unwrap() };
		let (client_conn, server_conn) = tokio::join!(async { connecting.await.unwrap() }, accept);

		(client_conn, server_conn, [client, server], gate)
	}

	/// Rejecting a CONNECT cannot accidentally advertise success or an interim response.
	#[tokio::test]
	async fn a_rejection_refuses_non_error_statuses() {
		use futures::FutureExt;
		for status in [
			http::StatusCode::CONTINUE,
			http::StatusCode::OK,
			http::StatusCode::NO_CONTENT,
			http::StatusCode::FOUND,
		] {
			let (client, server, _endpoints) = pair().await;
			let (client, ()) = tokio::join!(
				Session::connect(client, url::Url::parse("https://localhost/").unwrap()),
				async {
					let request = crate::quic::web::Request::accept(server).await.unwrap();
					assert!(matches!(request.reject(status).now_or_never(),
						Some(Err(crate::quic::web::ServerError::ConnectError(crate::quic::web::ConnectError::ErrorStatus(actual)))) if actual == status));
				},
			);
			assert!(client.is_err());
		}
	}

	/// An empty FIN needs no read; payload remains readable before the close watch ends.
	#[tokio::test]
	async fn clean_fin_ends_close_watch() {
		for framed in [false, true] {
			for payload in [Bytes::new(), Bytes::from(vec![0xa5; 64 * 1024 + 1])] {
				let (client, server, _endpoints) = pair().await;
				let (client, server) = if framed {
					let (client, server) = tokio::join!(
						Session::connect(client, url::Url::parse("https://localhost/").unwrap()),
						async { crate::quic::web::Request::accept(server).await.unwrap().ok().await },
					);
					(client.unwrap(), server.unwrap())
				} else {
					(Session::raw(client), Session::raw(server))
				};
				let mut send = client.open_uni(Default::default()).await.unwrap();
				send.write_all(payload.clone()).await.unwrap();
				let finished = send.finish();
				let mut recv = server.accept_uni().await.unwrap();
				if !payload.is_empty() {
					assert_eq!(recv.read_to_end(payload.len()).await.unwrap(), payload);
				}
				// The FIN is acknowledged, so checking close needs no timing assumption.
				assert!(finished.await.unwrap().is_none());
				assert!(matches!(recv.poll_closed(&Waiter::noop()), Poll::Ready(Ok(()))));
				assert!(recv.read_chunk(1).await.unwrap().is_none());
			}
		}
	}

	/// Both a WebTransport session over `client`/`server`, or both raw QUIC.
	async fn sessions(
		framed: bool,
		client: crate::quic::driver::connection::Connection,
		server: crate::quic::driver::connection::Connection,
	) -> (Session, Session) {
		if !framed {
			return (Session::raw(client), Session::raw(server));
		}
		let (client, server) = tokio::join!(
			Session::connect(client, url::Url::parse("https://localhost/").unwrap()),
			async { crate::quic::web::Request::accept(server).await.unwrap().ok().await },
		);
		(client.unwrap(), server.unwrap())
	}

	/// Both session kinds report the connection's stream credit, blocked once an open waits for
	/// the peer to grant more.
	#[tokio::test]
	async fn stream_credit_follows_the_connection() {
		use moq_net::transport::{Dir, StreamCredit, poll::Session as _};

		for framed in [false, true] {
			let mut server_transport = crate::quic::driver::transport::Config::default();
			server_transport.limits.max_streams = std::num::NonZeroU64::new(4);
			let (client, server, _endpoints, _gate) =
				gated_pair_with_transport(server_transport, Default::default()).await;
			let (client, _server) = sessions(framed, client, server).await;

			// HTTP/3 spends some of the credit on its own streams before the session starts.
			let start = client.stream_credit(Dir::Bi).unwrap();
			assert!(start.available > 0 && !start.blocked, "framed: {framed}");
			let mut opened = Vec::new();
			for left in (0..start.available).rev() {
				opened.push(client.open_bi(send::Options::default()).await.unwrap());
				assert_eq!(client.stream_credit(Dir::Bi), Some(StreamCredit::new(left, false)));
			}
			let pending = moq_net::transport::poll::Session::poll_open_bi(
				&client,
				&Waiter::noop(),
				moq_net::transport::poll::SendOptions::default(),
			);
			assert!(pending.is_pending(), "framed: {framed}");
			assert_eq!(client.stream_credit(Dir::Bi), Some(StreamCredit::new(0, true)));
			assert!(!client.stream_credit(Dir::Uni).unwrap().blocked, "each direction blocks alone");
		}
	}

	/// Once a read met the peer's reset, the close watch keeps reporting it rather than a clean end.
	#[tokio::test]
	async fn a_read_reset_stays_reset_for_the_close_watch() {
		for framed in [false, true] {
			let (client, server, _endpoints) = pair().await;
			let (client, server) = sessions(framed, client, server).await;
			let mut send = client.open_uni(Default::default()).await.unwrap();
			send.write_all(&b"partial"[..]).await.unwrap();
			let mut recv = server.accept_uni().await.unwrap();
			send.reset(7);
			loop {
				match recv.read_chunk(usize::MAX).await {
					Ok(Some(_)) => continue,
					Err(crate::quic::web::recv::Error::Reset(7)) => break,
					other => panic!("expected the reset, got {other:?}"),
				}
			}
			for _ in 0..2 {
				assert!(matches!(
					recv.closed().await,
					Err(crate::quic::web::recv::Error::Reset(7))
				));
			}
		}
	}

	/// Leased bytes handed back unread hold the close watch off, rather than being discarded
	/// behind a clean end; once read through a lease, the stream ends.
	#[tokio::test]
	async fn an_unread_lease_holds_the_close_watch_off() {
		for framed in [false, true] {
			let (client, server, _endpoints) = pair().await;
			let (client, server) = sessions(framed, client, server).await;
			let mut send = client.open_uni(Default::default()).await.unwrap();
			send.write_all(&b"leased"[..]).await.unwrap();
			let finished = send.finish();
			let mut recv = server.accept_uni().await.unwrap();
			// The FIN is acknowledged, so it has arrived and only the lease holds the end off.
			assert!(finished.await.unwrap().is_none());

			let waiter = Waiter::noop();
			let Poll::Ready(lease) = recv.poll_lease(&waiter, 6) else {
				panic!("acknowledged bytes are leasable");
			};
			let lease = lease.unwrap();
			assert!(lease.is_fin());
			drop(lease);
			assert!(recv.poll_closed(&Waiter::noop()).is_pending());

			let Poll::Ready(lease) = recv.poll_lease(&waiter, 6) else {
				panic!("restored bytes are leasable");
			};
			let mut lease = lease.unwrap();
			assert_eq!(lease.copy_to_bytes(6), &b"leased"[..]);
			lease.release();
			assert!(matches!(recv.poll_closed(&Waiter::noop()), Poll::Ready(Ok(()))));
		}
	}

	/// A datagram that names no session, or another one, is dropped rather than ending the session.
	#[tokio::test]
	async fn a_stray_datagram_is_dropped() {
		let (client, server, _endpoints) = pair().await;
		let (client, server) = sessions(true, client, server).await;
		// Session IDs are client bidi stream IDs, so 1 is never one; an empty datagram names none.
		for stray in [&[0x01, b'x'][..], &[][..]] {
			client.connection().send_datagram(stray, Default::default()).unwrap();
		}
		client.send_datagram(&b"ok"[..], Default::default()).unwrap();
		let datagram = kio::clock::timeout(Duration::from_secs(10), server.recv_datagram())
			.await
			.expect("no datagram arrived")
			.unwrap();
		assert_eq!(datagram, &b"ok"[..]);
	}

	/// Accepted headers buffered before the connection closed precede its terminal error.
	#[tokio::test]
	async fn buffered_stream_headers_precede_the_close() {
		let (client, server, _endpoints) = pair().await;
		let (client, server) = tokio::join!(
			Session::connect(client, url::Url::parse("https://localhost/").unwrap()),
			async { crate::quic::web::Request::accept(server).await.unwrap().ok().await },
		);
		let (client, server) = (client.unwrap(), server.unwrap());
		let mut uni = client.open_uni(Default::default()).await.unwrap();
		uni.write_all(&b"uni"[..]).await.unwrap();
		assert!(uni.finish().await.unwrap().is_none());
		let (mut bi, _recv) = client.open_bi(Default::default()).await.unwrap();
		bi.write_all(&b"bi"[..]).await.unwrap();
		assert!(bi.finish().await.unwrap().is_none());
		client.close(0, "done");
		server.closed().await;
		let mut uni = server.accept_uni().await.unwrap();
		assert_eq!(uni.read_to_end(3).await.unwrap(), b"uni");
		let (_send, mut bi) = server.accept_bi().await.unwrap();
		assert_eq!(bi.read_to_end(2).await.unwrap(), b"bi");
		assert!(server.accept_uni().await.is_err());
		assert!(server.accept_bi().await.is_err());
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
		server.close(2, "unauthorized");
		drop(server);
		gate.attempted.notified().await;
		tokio::time::advance(Duration::from_secs(1)).await;
		tokio::task::yield_now().await;
		assert!(
			conn.close_reason().is_none(),
			"closed before the capsule was acknowledged"
		);

		if cancel {
			conn.close(0u32.into(), "drain deadline");
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
					crate::quic::driver::recv::Error::Reset(code)
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
		let (mut send, _recv) = client.open_bi(Default::default()).await.unwrap();
		send.write_all(&[0x21, 0x00][..]).await.unwrap();
		let (_send, recv) = server.accept_bi().await.unwrap();

		client.close(0u32.into(), "done");
		assert!(Session::read_capsules(&server, recv).await.is_none());
		assert!(!logs_contain("failed to read capsule"), "an ordinary close warned");
	}

	/// A CONNECT stream that ends mid-frame on a live connection still warns.
	#[tokio::test]
	#[tracing_test::traced_test]
	async fn capsule_read_truncated_warns() {
		let (client, server, _endpoints) = pair().await;

		// A DATA frame that promises 5 bytes and carries 1.
		let (mut send, _recv) = client.open_bi(Default::default()).await.unwrap();
		send.write_all(&[0x00, 0x05, 0x01][..]).await.unwrap();
		send.finish();
		let (_send, recv) = server.accept_bi().await.unwrap();

		assert!(Session::read_capsules(&server, recv).await.is_none());
		assert!(
			logs_contain("failed to read capsule"),
			"a truncated capsule stream was not reported"
		);
	}

	#[tokio::test]
	async fn webtransport_accepts_a_buffered_uni_after_the_peer_close() {
		let (client, server, _endpoints) = pair().await;
		let mut send = client.open_uni(Default::default()).await.unwrap();
		// WebTransport stream type, session ID 0, then the application payload.
		send.write_all(&[0x40, 0x54, 0x00, b'l', b'a', b's', b't'][..])
			.await
			.unwrap();
		let send = send.finish();
		assert!(send.closed().await.unwrap().is_none());
		client.close(0u32.into(), "done");
		server.closed().await;
		let mut accept = SessionAccept::new(server, SESSION, Arc::new(CloseReason::new(false)), Vec::new());
		let mut recv = kio::wait(|waiter| accept.poll_accept_uni(waiter))
			.await
			.expect("buffered uni precedes the close");
		assert_eq!(recv.read_to_end(64).await.unwrap(), b"last");
		assert!(kio::wait(|waiter| accept.poll_accept_uni(waiter)).await.is_err());
	}

	/// A uni stream reset before its header arrived reports the reset, not a foreign session.
	#[tokio::test]
	async fn uni_reset_before_header() {
		let (client, server, _endpoints) = pair().await;

		// The stream type alone, so the reset also cuts off the session ID.
		let mut send = client.open_uni(Default::default()).await.unwrap();
		send.write_all(&[0x40, 0x54][..]).await.unwrap();
		send.reset(7u32.into());

		let recv = server.accept_uni().await.unwrap();
		let err = read_uni_header(recv).await.err().unwrap();
		assert_reset(err);
	}

	/// A bi stream reset before its header arrived reports the reset, not a foreign session.
	#[tokio::test]
	async fn bi_reset_before_header() {
		let (client, server, _endpoints) = pair().await;

		let (send, _recv) = client.open_bi(Default::default()).await.unwrap();
		send.reset(7u32.into());

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

		client.open_uni(Default::default()).await.unwrap().reset(7);
		let (send, _recv) = client.open_bi(Default::default()).await.unwrap();
		send.reset(8);

		let read = async {
			let mut uni = server.accept_uni().await.unwrap();
			let (_send, mut bi) = server.accept_bi().await.unwrap();
			(
				uni.read_chunk(usize::MAX).await.unwrap_err(),
				bi.read_chunk(usize::MAX).await.unwrap_err(),
			)
		};
		// Without the header the server never surfaces the streams, so bound the wait.
		let (uni, bi) = kio::clock::timeout(Duration::from_secs(10), read).await.unwrap();
		assert!(matches!(uni, crate::quic::web::recv::Error::Reset(7)), "{uni:?}");
		assert!(matches!(bi, crate::quic::web::recv::Error::Reset(8)), "{bi:?}");
	}

	/// A stream dropped before `finish` is reset with WebTransport code 0, still after its
	/// header, so the peer neither reads the truncated data as complete nor loses the reset.
	#[tokio::test]
	async fn drop_resets_after_header() {
		let (client_conn, server_conn, _endpoints) = pair().await;
		let (client, server) = tokio::join!(
			Session::connect(client_conn, url::Url::parse("https://localhost/").unwrap()),
			async { crate::quic::web::Request::accept(server_conn).await.unwrap().ok().await },
		);
		let (client, server) = (client.unwrap(), server.unwrap());

		let mut uni = client.open_uni(Default::default()).await.unwrap();
		uni.write_all(&b"truncated"[..]).await.unwrap();
		drop(uni);
		let (mut send, mut client_bi) = client.open_bi(Default::default()).await.unwrap();
		send.write_all(&b"request"[..]).await.unwrap();
		drop(send.finish());

		let read = async {
			let mut uni = server.accept_uni().await.unwrap();
			// The accepted half carries no header of its own, but still needs the code mapped.
			let (send, mut bi) = server.accept_bi().await.unwrap();
			assert_eq!(bi.read_to_end(usize::MAX).await.unwrap(), b"request");
			drop(send);
			let uni = loop {
				match uni.read_chunk(usize::MAX).await {
					Ok(Some(_)) => {}
					res => break res,
				}
			};
			(uni, client_bi.read_chunk(usize::MAX).await)
		};
		// Without the header the server never surfaces the stream, so bound the wait.
		let (uni, bi) = kio::clock::timeout(Duration::from_secs(10), read).await.unwrap();
		assert!(matches!(uni, Err(crate::quic::web::recv::Error::Reset(0))), "{uni:?}");
		assert!(matches!(bi, Err(crate::quic::web::recv::Error::Reset(0))), "{bi:?}");
	}

	/// A stream dropped before its end is stopped with WebTransport code 0, not the driver's raw
	/// 0, which falls outside the HTTP/3 range and reaches the writer as an invalid stop.
	#[tokio::test]
	async fn drop_stops_with_mapped_code() {
		let (client_conn, server_conn, _endpoints) = pair().await;
		let (client, server) = tokio::join!(
			Session::connect(client_conn, url::Url::parse("https://localhost/").unwrap()),
			async { crate::quic::web::Request::accept(server_conn).await.unwrap().ok().await },
		);
		let (client, server) = (client.unwrap(), server.unwrap());

		let mut uni = client.open_uni(Default::default()).await.unwrap();
		uni.write_all(&b"unread"[..]).await.unwrap();
		let (mut bi, _client_recv) = client.open_bi(Default::default()).await.unwrap();
		bi.write_all(&b"unread"[..]).await.unwrap();

		let mut recv = server.accept_uni().await.unwrap();
		recv.read_chunk(usize::MAX).await.unwrap();
		drop(recv);
		let (_server_send, mut recv) = server.accept_bi().await.unwrap();
		recv.read_chunk(usize::MAX).await.unwrap();
		drop(recv);

		let stopped = |mut send: SendStream| async move {
			loop {
				if let Err(err) = send.write(&mut &b"more"[..]).await {
					return err;
				}
			}
		};
		let (uni, bi) = kio::clock::timeout(Duration::from_secs(10), async {
			tokio::join!(stopped(uni), stopped(bi))
		})
		.await
		.unwrap();
		assert!(matches!(uni, crate::quic::web::send::Error::Stopped(0)), "{uni:?}");
		assert!(matches!(bi, crate::quic::web::send::Error::Stopped(0)), "{bi:?}");
	}

	/// A stream that names another session is dropped, and the session's own stream after it
	/// still arrives.
	#[tokio::test]
	async fn uni_other_session() {
		let (client, server, _endpoints) = pair().await;

		for header in [[0x40, 0x54, 0x04], [0x40, 0x54, 0x00]] {
			let mut send = client.open_uni(Default::default()).await.unwrap();
			send.write_all(&header[..]).await.unwrap();
			send.write_all(&header[2..]).await.unwrap();
			send.finish();
		}

		let mut accept = SessionAccept::new(server, SESSION, Arc::new(CloseReason::new(false)), Vec::new());
		let mut recv = kio::wait(|waiter| accept.poll_accept_uni(waiter)).await.unwrap();
		assert_eq!(recv.read_to_end(64).await.unwrap(), [0x00]);
	}

	/// Run the HTTP/3 handshake on both ends once `before` has opened the client's streams that
	/// precede its control stream.
	async fn handshake_after(
		before: impl AsyncFnOnce(&crate::quic::driver::connection::Connection) -> Vec<crate::quic::driver::send::Stream>,
	) -> (
		Session,
		Session,
		Vec<crate::quic::driver::send::Stream>,
		[crate::quic::driver::endpoint::Endpoint; 2],
	) {
		let (client_conn, server_conn, endpoints) = pair().await;
		let early = before(&client_conn).await;
		let (client, server) = tokio::join!(
			Session::connect(client_conn, url::Url::parse("https://localhost/").unwrap()),
			async { crate::quic::web::Request::accept(server_conn).await.unwrap().ok().await },
		);
		(client.unwrap(), server.unwrap(), early, endpoints)
	}

	/// A QPACK stream, or one of an unknown type, ahead of the control stream neither fails nor
	/// stalls the handshake, and the QPACK stream stays open.
	#[tokio::test]
	async fn handshake_takes_qpack_and_unknown_streams_first() {
		let (client, server, _early, _endpoints) = handshake_after(async |conn| {
			let mut streams = Vec::new();
			// A QPACK encoder stream, then a reserved (grease) type.
			for header in [&[0x02][..], &[0x21]] {
				let mut send = conn.open_uni(Default::default()).await.unwrap();
				send.write_all(header).await.unwrap();
				streams.push(send);
			}
			streams
		})
		.await;

		let mut send = client.open_uni(Default::default()).await.unwrap();
		send.write_all(&b"after"[..]).await.unwrap();
		drop(send.finish());
		let mut recv = server.accept_uni().await.unwrap();
		assert_eq!(recv.read_to_end(64).await.unwrap(), b"after");

		// Closing the QPACK stream would end the peer's HTTP/3 connection, so the session holds it.
		let accept = server.accept.as_ref().unwrap().lock().unwrap();
		assert!(accept.qpack_encoder.is_some(), "the QPACK stream was dropped");
	}

	/// A stream that stalls partway through its header does not hold off the control stream
	/// behind it, and still reaches the session once the rest arrives.
	#[tokio::test]
	async fn handshake_passes_a_stalled_header() {
		let (_client, server, mut early, _endpoints) = handshake_after(async |conn| {
			let mut send = conn.open_uni(Default::default()).await.unwrap();
			// The first byte of the two-byte WebTransport stream type.
			send.write_all(&[0x40][..]).await.unwrap();
			vec![send]
		})
		.await;

		let mut send = early.remove(0);
		send.write_all(&[0x54, 0x00, b'l', b'a', b't', b'e'][..]).await.unwrap();
		send.finish();
		let mut recv = server.accept_uni().await.unwrap();
		assert_eq!(recv.read_to_end(64).await.unwrap(), b"late");
	}

	/// A WebTransport stream an eager client opens before its control stream reaches the session.
	#[tokio::test]
	async fn handshake_keeps_an_early_webtransport_stream() {
		let (_client, server, _early, _endpoints) = handshake_after(async |conn| {
			let mut send = conn.open_uni(Default::default()).await.unwrap();
			send.write_all(&[0x40, 0x54, 0x00, b'e', b'a', b'r', b'l', b'y'][..])
				.await
				.unwrap();
			// A finished stream delivers its data and FIN without its handle.
			send.finish();
			Vec::new()
		})
		.await;

		let mut recv = server.accept_uni().await.unwrap();
		assert_eq!(recv.read_to_end(64).await.unwrap(), b"early");
	}

	/// A peer that closes before sending its control stream fails the handshake rather than
	/// leaving it waiting.
	#[tokio::test]
	async fn handshake_ends_with_the_connection() {
		let (client, server, _endpoints) = pair().await;
		let mut send = client.open_uni(Default::default()).await.unwrap();
		send.write_all(&[0x02][..]).await.unwrap();
		client.close(0u32.into(), "done");
		assert!(crate::quic::web::Request::accept(server).await.is_err());
	}
}
