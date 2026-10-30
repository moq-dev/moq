//! WebTransport over the worker's QUIC path, for browser peers.
//!
//! A browser cannot speak raw QUIC: it dials with ALPN `h3` and expects an
//! HTTP/3 CONNECT handshake before streams flow, with every stream and
//! datagram prefixed to name the session. [`Request::accept`] runs that
//! handshake over a [`Connection`] (settings exchange, CONNECT, subprotocol
//! selection) using [`web_transport_proto`]'s state machines, and
//! [`Request::respond`] yields a [`Session`].
//!
//! [`Session`] supports both raw QUIC and WebTransport on the worker:
//! [`Session::raw`] wraps a raw-QUIC connection in the same type with the
//! WebTransport layering disabled, so native peers and browsers run the same
//! machinery. Stream and session error codes map through the HTTP/3 error
//! space ([`web_transport_proto::error_to_http3`]) in web mode and pass
//! through untouched in raw mode, except that a received stream code in the
//! HTTP/3 range is unmapped, since older raw peers send that form.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::task::{Context, Poll, ready};

use bytes::{Buf, Bytes, BytesMut};
use web_transport_proto as proto;

use super::{Connection, Error, VarInt, recv, send};

/// The frame type WebTransport bidirectional streams lead with.
const FRAME_WEBTRANSPORT: u64 = 0x41;
/// The stream type WebTransport unidirectional streams lead with.
const STREAM_WEBTRANSPORT: u64 = 0x54;
/// H3 unidirectional stream types a peer legitimately opens and we must keep
/// alive without reading: the control stream and the QPACK pair.
const STREAM_CONTROL: u64 = 0x00;
const STREAM_QPACK_ENCODER: u64 = 0x02;
const STREAM_QPACK_DECODER: u64 = 0x03;
/// The HTTP/3 DATA frame carrying capsules on the CONNECT stream.
const FRAME_DATA: u64 = 0x00;
/// How long a graceful close waits for the peer to act on the
/// `CloseWebTransportSession` capsule before closing the connection itself.
const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(1);
/// How much of one HTTP/3 message to buffer while waiting for the rest.
///
/// A peer declares a frame's length before sending its body, and reading what
/// does arrive replenishes its flow-control credit, so an unauthenticated
/// client could otherwise announce an enormous SETTINGS, CONNECT, or capsule
/// frame and dribble it until the process runs out of memory. Everything
/// parsed here is a few hundred bytes; no real peer meets this.
const MESSAGE_LIMIT: usize = 64 * 1024;
/// How many unidirectional streams a peer may open before its control stream
/// arrives.
///
/// The handshake holds each arrival (QPACK, the control stream) or parks it as
/// an early WebTransport stream, so without a cap a peer that never sends a
/// control stream could open them without end. A real client opens three
/// before its CONNECT, plus whatever it pipelines.
const HANDSHAKE_STREAMS: usize = 64;
/// How many of a peer's critical streams to hold open once the session is
/// running. HTTP/3 defines one control stream and two QPACK streams; the
/// slack is for a peer that greases, and anything past it is dropped rather
/// than retained.
const HELD_STREAMS: usize = 8;
/// `H3_GENERAL_PROTOCOL_ERROR`, for closing a connection whose HTTP/3
/// handshake never produced a session.
const H3_GENERAL_PROTOCOL_ERROR: u64 = 0x0101;
/// `H3_NO_ERROR`, for closing a connection that did what it came to do: a
/// rejection the peer has been told about.
const H3_NO_ERROR: u64 = 0x0100;

/// An incoming WebTransport handshake: the CONNECT request, ready to answer.
///
/// Produced by [`accept`](Self::accept) on a connection whose ALPN negotiated
/// `h3`. Answer with [`respond`](Self::respond) (or [`ok`](Self::ok)) to get
/// the [`Session`], or [`reject`](Self::reject) to refuse it.
pub struct Request {
	conn: Connection,
	/// Closes the connection on every way out of here that is not an answer.
	guard: Guard,
	request: proto::ConnectRequest,
	send: send::Stream,
	recv: recv::Stream,
	/// The peer's control and QPACK streams, held open for the session's
	/// life: closing them reads as tearing the H3 connection down.
	held: Vec<recv::Stream>,
	/// Our control stream, same deal.
	control: send::Stream,
	/// WebTransport streams a pipelining peer opened before its CONNECT was
	/// answered, headers consumed, keyed by the session id they claimed.
	early: Vec<(u64, recv::Stream)>,
	/// Streams that arrived during the handshake and are still mid-header;
	/// the session goes on classifying them.
	pending: Vec<PendingUni>,
}

/// Closes a connection whose handshake was abandoned, disarmed once the peer
/// has an answer.
///
/// Dropping a [`Connection`] does not close QUIC: the endpoint keeps it, its
/// routes, and its driver task until the driver sees a terminal state, and the
/// backlog stopped counting it at accept. The peer picks the path it asks for
/// and the subprotocols it offers, so it decides which of [`Request::respond`]'s
/// early returns the server takes; a guard covers every way out rather than
/// each one remembering.
struct Guard {
	conn: Option<Connection>,
}

impl Guard {
	fn new(conn: Connection) -> Self {
		Self { conn: Some(conn) }
	}

	/// The peer got an answer, so the connection is the answer's to close.
	fn disarm(&mut self) {
		self.conn = None;
	}

	fn close(&mut self, reason: &str) {
		if let Some(conn) = self.conn.take() {
			conn.close_code(H3_GENERAL_PROTOCOL_ERROR, reason);
		}
	}
}

impl Drop for Guard {
	fn drop(&mut self) {
		self.close("webtransport handshake abandoned");
	}
}

impl Request {
	/// Run the server side of the HTTP/3 handshake: exchange SETTINGS, then
	/// take the CONNECT request.
	///
	/// Everything the session later spawns runs on the worker already driving
	/// `conn`. No deadline here: a caller bounding the handshake, say by
	/// [`Info::deadline`](super::handshake::Info::deadline), drops this
	/// future, which closes the connection with an HTTP/3 error, even if it
	/// was never polled.
	pub fn accept(conn: Connection) -> impl Future<Output = Result<Self, Error>> {
		// The endpoint and driver retain this connection after the future is
		// dropped. Own its close before returning, not at the first poll, so a
		// deadline that has already passed cannot drop the future unguarded.
		// The returned Request takes over that duty once the handshake succeeds.
		let mut guard = Guard::new(conn.clone());
		async move {
			match Self::handshake(conn).await {
				Ok(request) => {
					guard.disarm();
					Ok(request)
				}
				Err(err) => {
					guard.close(&err.to_string());
					Err(err)
				}
			}
		}
	}

	async fn handshake(mut conn: Connection) -> Result<Self, Error> {
		// Our control stream: the SETTINGS advertising WebTransport support.
		let mut control = conn.open_uni(Default::default()).await?;
		let mut settings = proto::Settings::default();
		settings.enable_webtransport(1);
		let mut buf = Vec::new();
		settings.encode(&mut buf);
		write_all(&mut control, &buf).await?;

		// The peer's control stream carries its SETTINGS, but its QPACK
		// streams race it, and an eager client's WebTransport streams can
		// arrive ahead of everything, so classify every arrival at once.
		// Taking them one at a time would let a stream that sends its type
		// byte and then stalls hold off a control stream that has already
		// fully arrived, for as long as the peer keeps the connection alive.
		let mut pending: Vec<PendingUni> = Vec::new();
		let mut held = Vec::new();
		let mut early = Vec::new();
		// Every arrival counts, not just the ones kept: a stream of unknown
		// type is dropped here, and a dropped stream returns its credit, so
		// counting what we retain would let a peer loop this forever while
		// never sending a control stream.
		let mut arrivals = 0usize;
		let mut peer_control = None;
		std::future::poll_fn(|cx| {
			loop {
				// Stop adopting past the cap, but do not give up on what is
				// already here: the control stream may be sitting at the head
				// of a queue a pipelining peer filled behind it, and refusing
				// that peer is the bug the cap is not for. The one that tips
				// it over is kept rather than dropped, since dropping it would
				// cancel a legitimate stream on a handshake that then succeeds.
				let mut over = false;
				while !over {
					match conn.poll_accept_uni(cx) {
						Poll::Ready(Ok(recv)) => {
							arrivals += 1;
							pending.push(PendingUni::new(recv));
							over = arrivals > HANDSHAKE_STREAMS;
						}
						Poll::Ready(Err(err)) => return Poll::Ready(Err(err)),
						Poll::Pending => break,
					}
				}

				// Each mid-header stream parks the caller in its own stream's
				// waiters, so progress on any of them re-polls us.
				let mut progressed = false;
				let mut index = 0;
				while index < pending.len() {
					let Poll::Ready(class) = pending[index].poll_classify(cx) else {
						index += 1;
						continue;
					};
					// Whatever was last takes this slot, so it is polled by
					// this same pass rather than waited for.
					let stream = pending.swap_remove(index);
					progressed = true;
					match class {
						UniClass::Control => {
							peer_control = Some(stream.recv);
							return Poll::Ready(Ok(()));
						}
						UniClass::Qpack => held.push(stream.recv),
						UniClass::Web(session) => early.push((session, stream.recv)),
						UniClass::Unknown => {}
					}
				}

				// Only now, with everything adopted classified: the peer sent
				// this many streams and none of them was a control stream.
				if over {
					return Poll::Ready(Err(Error::Web("too many streams before the control stream".into())));
				}
				if !progressed {
					return Poll::Pending;
				}
			}
		})
		.await?;

		let mut peer_control = peer_control.expect("the loop only ends with a control stream");
		let settings = read_settings(&mut peer_control).await?;
		if settings.supports_webtransport() == 0 {
			return Err(Error::Web("peer does not support WebTransport".into()));
		}
		held.push(peer_control);

		// The CONNECT request rides the client's first bidirectional stream,
		// which arrives first: QUIC creates lower-numbered streams
		// implicitly, and the accept queue is in id order.
		let (send, mut recv) = conn.accept_bi().await?;
		let request = read_connect(&mut recv).await?;

		Ok(Self {
			guard: Guard::new(conn.clone()),
			conn,
			request,
			send,
			recv,
			held,
			control,
			early,
			pending,
		})
	}

	/// The URL the peer connected to; the path and query are what a server
	/// routes and authenticates on. The `url` crate is
	/// [re-exported](crate::url) so naming this type needs no dependency of
	/// your own.
	pub fn url(&self) -> &url::Url {
		&self.request.url
	}

	/// The subprotocols the peer offered, the WebTransport equivalent of ALPN.
	/// Pick one and name it in [`respond`](Self::respond).
	pub fn protocols(&self) -> &[String] {
		&self.request.protocols
	}

	/// Accept with a `200`, answering as `response` describes.
	pub async fn respond(mut self, response: Response) -> Result<Session, Error> {
		let Response { protocol } = response;

		let mut encoded = proto::ConnectResponse::OK;
		if let Some(protocol) = &protocol {
			if !self.request.protocols.iter().any(|offered| offered == protocol) {
				return Err(Error::Web(format!("subprotocol {protocol:?} was not offered")));
			}
			encoded = encoded.with_protocol(protocol);
		}
		let mut buf = Vec::new();
		encoded.encode(&mut buf).map_err(|err| Error::Web(err.to_string()))?;
		write_all(&mut self.send, &buf).await?;
		self.guard.disarm();

		Ok(Session::establish(self, protocol))
	}

	/// Accept with a `200` and no subprotocol.
	pub async fn ok(self) -> Result<Session, Error> {
		self.respond(Response::default()).await
	}

	/// Refuse with `reason`, ending the handshake.
	///
	/// Returns once the peer has the response, or after a grace period if it
	/// never acknowledges one, and closes the connection deliberately. The
	/// HTTP/3 critical streams (the peer's control and QPACK streams, and
	/// ours) stay open until then: RFC 9114 makes closing one a connection
	/// error, so tearing them down here would show the peer an H3 failure
	/// instead of the status it was sent.
	pub async fn reject(mut self, reason: Rejected) -> Result<(), Error> {
		let response = proto::ConnectResponse::new(reason.status());
		let mut buf = Vec::new();
		response.encode(&mut buf).map_err(|err| Error::Web(err.to_string()))?;
		write_all(&mut self.send, &buf).await?;
		let mut send = self.send.finish();

		// The guard stays armed across the wait below. Cancelling this future
		// mid-grace would otherwise skip the deliberate close and leak the
		// connection, which is the very thing the guard is here to prevent.
		let mut deadline = self.conn.owner().after(CLOSE_GRACE);
		kio::wait(|waiter| {
			let cx = waiter.context();
			if send.poll_stopped(&cx).is_ready() {
				return Poll::Ready(());
			}
			deadline.poll(waiter)
		})
		.await;

		// The peer has the response, so this close is the deliberate one; the
		// guard's abrupt `H3_GENERAL_PROTOCOL_ERROR` would have raced it out.
		self.guard.disarm();
		self.conn.close_code(H3_NO_ERROR, "");
		Ok(())
	}
}

/// How to answer a CONNECT the server is accepting.
///
/// Built with [`default`](Self::default) and the setters below, so a knob
/// added later stays additive.
#[derive(Clone, Debug, Default)]
pub struct Response {
	protocol: Option<String>,
}

impl Response {
	/// Select a subprotocol from the ones the peer
	/// [offered](Request::protocols), the WebTransport equivalent of ALPN.
	/// Answering with one the peer did not offer is an error.
	pub fn with_protocol(mut self, protocol: impl Into<String>) -> Self {
		self.protocol = Some(protocol.into());
		self
	}
}

/// Why a CONNECT is being refused.
///
/// Named rather than numeric so callers need no HTTP crate of their own, and
/// so this stays a contract of moq-uring's rather than of whichever `http`
/// version it happens to build against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Rejected {
	/// The peer is not allowed here (`401`).
	Unauthorized,
	/// The peer is known and still not allowed here (`403`).
	Forbidden,
	/// Nothing is served at the requested path (`404`).
	NotFound,
	/// The request was malformed (`400`).
	BadRequest,
	/// The server cannot take it right now (`503`).
	Unavailable,
}

impl Rejected {
	fn status(self) -> http::StatusCode {
		match self {
			Self::Unauthorized => http::StatusCode::UNAUTHORIZED,
			Self::Forbidden => http::StatusCode::FORBIDDEN,
			Self::NotFound => http::StatusCode::NOT_FOUND,
			Self::BadRequest => http::StatusCode::BAD_REQUEST,
			Self::Unavailable => http::StatusCode::SERVICE_UNAVAILABLE,
		}
	}
}

impl std::fmt::Debug for Request {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Request").field("url", &self.request.url).finish()
	}
}

/// The WebTransport layering shared by a session's clones.
struct Web {
	/// The CONNECT stream's id: what every stream header and datagram carries.
	session_id: u64,
	/// Precomputed per-kind prefixes carrying the session id.
	header_uni: Bytes,
	header_bi: Bytes,
	header_datagram: Bytes,
	state: RefCell<State>,
	/// The session's terminal error, in WebTransport code space: a received
	/// `CloseWebTransportSession` capsule, or the local [`Session::close`].
	/// First writer wins.
	closed: RefCell<Option<Error>>,
}

struct State {
	/// Incoming streams whose headers are still being read.
	pending_uni: Vec<PendingUni>,
	pending_bi: Vec<PendingBi>,
	/// Classified application streams nobody has accepted yet.
	ready_uni: VecDeque<recv::Stream>,
	ready_bi: VecDeque<(send::Stream, recv::Stream)>,
	/// Streams held open, never read: control and QPACK, ours and theirs.
	held_recv: Vec<recv::Stream>,
	_control_send: Option<send::Stream>,
	/// The CONNECT stream's send half; taken exactly once by the close path.
	connect_send: Option<send::Stream>,
}

/// A MoQ transport over the worker: raw QUIC, or a WebTransport session.
///
/// [`Request::respond`] builds the web
/// flavor; [`Session::raw`] wraps a raw-QUIC [`Connection`] with the layering
/// disabled. Clones share the session; dropping the last closes the connection.
pub struct Session {
	// Capsule tasks retain Web/Connection, but only public session handles own this.
	handles: Rc<()>,
	conn: Connection,
	web: Option<Rc<Web>>,
	/// What [`protocol`](web_transport_trait::poll::Session::protocol)
	/// reports: the selected subprotocol in web mode, the ALPN in raw mode.
	protocol: Option<String>,
	/// Streams opened but still writing their WebTransport header, handed out
	/// only once it is all written, so a reset always delivers it.
	opening_uni: Option<(SendStream, Bytes)>,
	opening_bi: Option<(SendStream, RecvStream, Bytes)>,
}

impl Session {
	/// The wire counters the connection updates as it sends and receives.
	pub(crate) fn wire(&self) -> moq_net::transport::Wire {
		self.conn.wire()
	}

	/// The target the connection probes up to, or `None` when it cannot probe.
	pub(crate) fn probe(&self) -> Option<moq_net::transport::Probe> {
		self.conn.probe()
	}

	/// Receive streams evicted to free a stuck receive window.
	pub(crate) fn streams_evicted(&self) -> u64 {
		self.conn.streams_evicted()
	}

	/// Open a send group at `priority`, for streams to join with
	/// [`SendStream::set_group`]. The HTTP/3 streams stay outside every
	/// group, so they go first.
	pub fn send_group(&self, priority: i64) -> send::Group {
		self.conn.send_group(priority)
	}

	/// Send a datagram scheduled by `options`, like one of its group's streams.
	///
	/// See [`Connection::send_datagram`].
	pub fn send_datagram(&self, payload: Bytes, options: send::Options<'_>) -> Result<(), Error> {
		let Some(web) = &self.web else {
			return Ok(self.conn.send_datagram(payload, options)?);
		};
		// The session id goes ahead of the payload as its own chunk, so neither is copied.
		let framed = web.header_datagram.clone().chain(payload);
		self.conn
			.send_datagram(framed, options)
			.map_err(|err| unmap_err(err.into()))
	}

	/// Wrap a raw-QUIC connection in a session, with no
	/// WebTransport layering: streams and datagrams pass through untouched.
	pub fn raw(conn: Connection) -> Self {
		let protocol = conn.protocol().map(str::to_owned);
		Self {
			handles: Rc::new(()),
			conn,
			web: None,
			protocol,
			opening_uni: None,
			opening_bi: None,
		}
	}

	/// Change the underlying connection's flow-control limits; see
	/// [`Connection::set_limits`].
	pub fn set_limits(&self, limits: super::Limits) -> Result<(), super::Error> {
		self.conn.set_limits(limits)
	}

	/// Assemble the web flavor and spawn its capsule reader.
	fn establish(request: Request, protocol: Option<String>) -> Self {
		let Request {
			conn,
			guard: _,
			request: _,
			send,
			recv,
			held,
			control,
			early,
			pending,
		} = request;

		let session_id = u64::from(send.id());
		let mut header_uni = Vec::new();
		encode_varint(STREAM_WEBTRANSPORT, &mut header_uni);
		encode_varint(session_id, &mut header_uni);
		let mut header_bi = Vec::new();
		encode_varint(FRAME_WEBTRANSPORT, &mut header_bi);
		encode_varint(session_id, &mut header_bi);
		let mut header_datagram = Vec::new();
		encode_varint(session_id, &mut header_datagram);

		// A pipelining peer's streams claimed a session id before this one
		// existed; only its own id could have been meant.
		let ready_uni = early
			.into_iter()
			.filter_map(|(session, recv)| (session == session_id).then_some(recv))
			.collect();

		let web = Rc::new(Web {
			session_id,
			header_uni: header_uni.into(),
			header_bi: header_bi.into(),
			header_datagram: header_datagram.into(),
			state: RefCell::new(State {
				// Still mid-header when the control stream turned up; the
				// session finishes classifying them, so a pipelined
				// WebTransport stream is not lost to the handshake ending.
				pending_uni: pending,
				pending_bi: Vec::new(),
				ready_uni,
				ready_bi: VecDeque::new(),
				held_recv: held,
				_control_send: Some(control),
				connect_send: Some(send),
			}),
			closed: RefCell::new(None),
		});

		// The peer signals session close with a capsule on the CONNECT
		// stream; read it so the close code survives the H3 mapping.
		let capsules = web.clone();
		let capsule_conn = conn.clone();
		conn.owner()
			.spawn(async move { read_capsules(capsules, capsule_conn, recv).await });

		Self {
			handles: Rc::new(()),
			conn,
			web: Some(web),
			protocol,
			opening_uni: None,
			opening_bi: None,
		}
	}

	/// Rewrite error codes out of the HTTP/3 mapping in web mode.
	fn map_err(&self, err: Error) -> Error {
		match self.web {
			Some(_) => unmap_err(err),
			None => err,
		}
	}
}

impl Clone for Session {
	fn clone(&self) -> Self {
		Self {
			handles: self.handles.clone(),
			conn: self.conn.clone(),
			web: self.web.clone(),
			protocol: self.protocol.clone(),
			opening_uni: None,
			opening_bi: None,
		}
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		if Rc::strong_count(&self.handles) == 1 {
			web_transport_trait::poll::Session::close(self, 0, "dropped");
		}
	}
}

impl std::fmt::Debug for Session {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Session")
			.field("web", &self.web.is_some())
			.field("protocol", &self.protocol)
			.finish()
	}
}

impl Session {
	pub(crate) fn poll_open_request(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<Option<(SendStream, RecvStream)>, Error>> {
		if self.opening_bi.is_none() {
			let Some((send, recv)) = self.conn.open_request().map_err(|err| self.map_err(err))? else {
				return Poll::Ready(Ok(None));
			};
			let web = self.web.is_some();
			let header = self.web.as_ref().map(|web| web.header_bi.clone()).unwrap_or_default();
			self.opening_bi = Some((SendStream::new(send, web), RecvStream::new(recv, web), header));
		}
		web_transport_trait::poll::Session::poll_open_bi(self, cx).map_ok(Some)
	}
}

impl web_transport_trait::poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;

	fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::RecvStream, Self::Error>> {
		let Some(web) = self.web.clone() else {
			let inner = ready!(self.conn.poll_accept_uni(cx))?;
			return Poll::Ready(Ok(RecvStream::new(inner, false)));
		};

		loop {
			if let Some(inner) = web.state.borrow_mut().ready_uni.pop_front() {
				return Poll::Ready(Ok(RecvStream::new(inner, true)));
			}

			// Adopt every new arrival, then try to classify the pending set.
			// Each mid-header stream parks the caller in its own stream's
			// waiters, so progress on any of them re-polls us.
			let ended = loop {
				match self.conn.poll_accept_uni(cx) {
					Poll::Ready(Ok(recv)) => web.state.borrow_mut().pending_uni.push(PendingUni::new(recv)),
					Poll::Ready(Err(err)) => break Some(err),
					Poll::Pending => break None,
				}
			};

			if !classify_uni(&web, cx) {
				// Buffered headers can finish after the connection error. Hand those
				// streams out before reporting that no more can arrive.
				if web.state.borrow().pending_uni.is_empty()
					&& let Some(err) = ended
				{
					return Poll::Ready(Err(unmap_err(err)));
				}
				return Poll::Pending;
			}
		}
	}

	fn poll_accept_bi(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		let Some(web) = self.web.clone() else {
			let (send, recv) = ready!(self.conn.poll_accept_bi(cx))?;
			return Poll::Ready(Ok((SendStream::new(send, false), RecvStream::new(recv, false))));
		};

		loop {
			if let Some((send, recv)) = web.state.borrow_mut().ready_bi.pop_front() {
				return Poll::Ready(Ok((SendStream::new(send, true), RecvStream::new(recv, true))));
			}

			loop {
				match self.conn.poll_accept_bi(cx) {
					Poll::Ready(Ok((send, recv))) => web.state.borrow_mut().pending_bi.push(PendingBi::new(send, recv)),
					Poll::Ready(Err(err)) => return Poll::Ready(Err(unmap_err(err))),
					Poll::Pending => break,
				}
			}

			if !classify_bi(&web, cx) {
				return Poll::Pending;
			}
		}
	}

	fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Self::SendStream, Self::Error>> {
		if self.opening_uni.is_none() {
			let inner = ready!(self.conn.poll_open_uni(cx, Default::default())).map_err(|err| self.map_err(err))?;
			let header = self.web.as_ref().map(|web| web.header_uni.clone()).unwrap_or_default();
			self.opening_uni = Some((SendStream::new(inner, self.web.is_some()), header));
		}
		let (send, header) = self.opening_uni.as_mut().expect("opening");
		let result = ready!(send.poll_header(cx, header));
		let (send, _) = self.opening_uni.take().expect("opening");
		Poll::Ready(result.map(|()| send))
	}

	fn poll_open_bi(
		&mut self,
		cx: &mut Context<'_>,
	) -> Poll<Result<(Self::SendStream, Self::RecvStream), Self::Error>> {
		if self.opening_bi.is_none() {
			let (send, recv) =
				ready!(self.conn.poll_open_bi(cx, Default::default())).map_err(|err| self.map_err(err))?;
			let web = self.web.is_some();
			let header = self.web.as_ref().map(|web| web.header_bi.clone()).unwrap_or_default();
			self.opening_bi = Some((SendStream::new(send, web), RecvStream::new(recv, web), header));
		}
		let (send, _, header) = self.opening_bi.as_mut().expect("opening");
		let result = ready!(send.poll_header(cx, header));
		let (send, recv, _) = self.opening_bi.take().expect("opening");
		Poll::Ready(result.map(|()| (send, recv)))
	}

	fn poll_send_datagram(&mut self, _: &mut Context<'_>, payload: &[u8]) -> Poll<Result<(), Self::Error>> {
		Poll::Ready(Self::send_datagram(
			self,
			Bytes::copy_from_slice(payload),
			Default::default(),
		))
	}

	fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, Self::Error>> {
		let Some(web) = self.web.clone() else {
			return self.conn.poll_recv_datagram(cx);
		};
		loop {
			let datagram = ready!(self.conn.poll_recv_datagram(cx)).map_err(unmap_err)?;
			// The prefix names the session; anything else is not ours.
			let mut peek: &[u8] = &datagram;
			match decode_varint(&mut peek) {
				Some(id) if id == web.session_id => {
					let start = datagram.len() - peek.len();
					return Poll::Ready(Ok(datagram.slice(start..)));
				}
				_ => tracing::debug!("dropping a datagram for an unknown session"),
			}
		}
	}

	fn max_datagram_size(&self) -> usize {
		let inner = self.conn.max_datagram_size().unwrap_or(0);
		match &self.web {
			Some(web) => inner.saturating_sub(web.header_datagram.len()),
			None => inner,
		}
	}

	fn protocol(&self) -> Option<&str> {
		self.protocol.as_deref()
	}

	fn close(&mut self, code: u32, reason: &str) {
		let Some(web) = &self.web else {
			return self.conn.close_code(u64::from(code), reason);
		};

		{
			let mut closed = web.closed.borrow_mut();
			if closed.is_some() {
				return;
			}
			*closed = Some(Error::App {
				code: u64::from(code),
				reason: reason.to_string(),
			});
		}

		let connect_send = web.state.borrow_mut().connect_send.take();
		let http3 = proto::error_to_http3(code);
		let Some(send) = connect_send else {
			self.conn.close_code(http3, reason);
			return;
		};

		// The capsule is what carries the code and reason to a browser
		// (`WebTransport.closed`); the connection close alone would lose the
		// reason and squash the code through the H3 mapping.
		let capsule = proto::Capsule::CloseWebTransportSession {
			code,
			reason: reason.to_string(),
		};
		let mut payload = Vec::new();
		capsule.encode(&mut payload);
		let mut frame = Vec::new();
		encode_varint(FRAME_DATA, &mut frame);
		encode_varint(payload.len() as u64, &mut frame);
		frame.extend_from_slice(&payload);

		// Finish the capsule and then give the peer a moment to act on it,
		// closing either way once the grace period is up. The write goes in
		// the task rather than here because flow control can take it in
		// pieces, and abandoning a partial frame would leave the browser with
		// neither the code nor the reason.
		let mut deadline = self.conn.owner().after(CLOSE_GRACE);
		let reason = reason.to_string();
		let mut conn = self.conn.clone();
		// The caller may drop every handle right after this, so the task holds
		// the H3 control and QPACK streams itself: RFC 9114 makes closing one a
		// connection error, which a browser reports instead of the capsule.
		let web = web.clone();
		self.conn.owner().spawn(async move {
			let mut frame = Bytes::from(frame);
			// Taken to finish once the frame is all written, and kept until the close.
			let mut send = Some(send);
			let mut finished = None;
			kio::wait(|waiter| {
				let cx = waiter.context();

				// Stop writing once the connection is gone; the deadline is the
				// only other thing that ends this.
				if conn.poll_closed(&cx).is_ready() {
					return Poll::Ready(());
				}
				if let Some(stream) = &mut send {
					while frame.has_remaining() {
						match stream.poll_write(&cx, &mut frame) {
							Poll::Ready(Ok(_)) => {}
							// The stream is unusable, so the connection close below
							// is all the peer is going to get.
							Poll::Ready(Err(_)) => return Poll::Ready(()),
							Poll::Pending => break,
						}
					}
					if !frame.has_remaining() {
						finished = send.take().map(send::Stream::finish);
					}
				}

				deadline.poll(waiter)
			})
			.await;
			conn.close_code(http3, &reason);
			// Only now, so the critical streams end with the connection.
			drop((web, finished));
		});
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Self::Error> {
		let err = ready!(self.conn.poll_closed(cx));
		let Some(web) = &self.web else {
			return Poll::Ready(err);
		};
		// A recorded close (capsule or local) is the truth; the connection
		// error is just its H3-mangled echo.
		if let Some(recorded) = web.closed.borrow().clone() {
			return Poll::Ready(recorded);
		}
		Poll::Ready(unmap_err(err))
	}

	fn stats(&self) -> impl web_transport_trait::Stats {
		self.conn.transport_stats()
	}
}

/// An outgoing stream, WebTransport-framed when its session is.
///
/// A web stream we opened is handed out with its header already written, so a
/// reset still delivers the header and the peer can tell which session the
/// reset belongs to.
pub struct SendStream {
	state: SendState,
	/// The end of the WebTransport header on a stream we opened, for a reset
	/// to deliver.
	header: Option<send::Offset>,
	web: bool,
}

/// Where the stream is, since `web_transport_trait` finishes and resets through `&mut self`.
enum SendState {
	Open(send::Stream),
	Finished(send::Finished),
	/// Reset, by us or by echoing the peer's stop.
	Reset,
}

impl SendStream {
	fn new(mut inner: send::Stream, web: bool) -> Self {
		// The driver's raw 0 reads to a browser as an HTTP/3 stream error rather than the
		// WebTransport cancellation that dropping a stream means. moq cancels subscriptions by
		// dropping, so this is the ordinary path.
		if web {
			inner.set_drop_reset(None, http3_code(proto::error_to_http3(0)));
		}
		Self {
			state: SendState::Open(inner),
			header: None,
			web,
		}
	}

	fn map(&self, err: impl Into<Error>) -> Error {
		match self.web {
			true => unmap_err(err.into()),
			false => unmap_legacy(err.into()),
		}
	}

	/// Set the priority within the stream's send group, or session-wide when ungrouped.
	pub fn set_priority(&mut self, priority: i64) {
		match &mut self.state {
			SendState::Open(stream) => stream.set_priority(priority),
			SendState::Finished(stream) => stream.set_priority(priority),
			SendState::Reset => {}
		}
	}

	/// Move the stream into a send group, or with `None` back out. See
	/// [`send::Stream::set_group`].
	pub fn set_group(&mut self, group: Option<&send::Group>) {
		match &mut self.state {
			SendState::Open(stream) => stream.set_group(group),
			SendState::Finished(stream) => stream.set_group(group),
			SendState::Reset => {}
		}
	}

	/// Write the WebTransport header of a stream we opened, noting where it
	/// ends once it is all written.
	fn poll_header(&mut self, cx: &mut Context<'_>, header: &mut Bytes) -> Poll<Result<(), Error>> {
		if !self.web {
			return Poll::Ready(Ok(()));
		}
		let SendState::Open(stream) = &mut self.state else {
			unreachable!("the header goes first");
		};
		while header.has_remaining() {
			if let Err(err) = ready!(stream.poll_write(cx, header)) {
				return Poll::Ready(Err(self.map(err)));
			}
		}
		let offset = stream.offset();
		stream.set_drop_reset(Some(offset), http3_code(proto::error_to_http3(0)));
		self.header = Some(offset);
		Poll::Ready(Ok(()))
	}

	fn poll_write_any(&mut self, cx: &mut Context<'_>, buf: &mut impl Buf) -> Poll<Result<usize, Error>> {
		let SendState::Open(stream) = &mut self.state else {
			return Poll::Ready(Err(Error::Quic("stream already finished".to_string())));
		};
		match ready!(stream.poll_write(cx, buf)) {
			Ok(n) => Poll::Ready(Ok(n)),
			Err(err) => Poll::Ready(Err(self.map(err))),
		}
	}
}

impl web_transport_trait::poll::SendStream for SendStream {
	type Error = Error;

	fn poll_write(&mut self, cx: &mut Context<'_>, mut buf: &[u8]) -> Poll<Result<usize, Self::Error>> {
		self.poll_write_any(cx, &mut buf)
	}

	fn poll_write_buf<B: Buf>(&mut self, cx: &mut Context<'_>, buf: &mut B) -> Poll<Result<usize, Self::Error>> {
		self.poll_write_any(cx, buf)
	}

	fn set_priority(&mut self, order: i32) {
		SendStream::set_priority(self, order.into());
	}

	fn finish(&mut self) -> Result<(), Self::Error> {
		let stream = match std::mem::replace(&mut self.state, SendState::Reset) {
			SendState::Open(stream) => stream,
			// Already finished or reset, so the FIN it wanted is out. A finished stream keeps
			// its handle, which `poll_closed` and `reset` still need.
			done => {
				self.state = done;
				return Ok(());
			}
		};
		// A STOP_SENDING beat us here. Carry the code like `poll_write` does, or
		// `moq_net::Error::from_transport` cannot decode a routine cancellation. Dropping the
		// stream echoes the code.
		if let Some(code) = stream.stop_code() {
			return Err(self.map(send::Error::Stopped(code)));
		}
		self.state = SendState::Finished(stream.finish());
		Ok(())
	}

	fn reset(&mut self, code: u32) {
		let code = match self.web {
			true => http3_code(proto::error_to_http3(code)),
			false => VarInt::from_u32(code),
		};
		match std::mem::replace(&mut self.state, SendState::Reset) {
			SendState::Open(stream) => match self.header {
				Some(header) => stream.reset_at(header, code),
				None => stream.reset(code),
			},
			SendState::Finished(stream) => match self.header {
				Some(header) => stream.reset_at(header, code),
				None => stream.reset(code),
			},
			SendState::Reset => {}
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let stopped = match &mut self.state {
			SendState::Open(stream) => ready!(stream.poll_stopped(cx)),
			SendState::Finished(stream) => ready!(stream.poll_stopped(cx)),
			SendState::Reset => return Poll::Ready(Ok(())),
		};
		Poll::Ready(match stopped {
			Ok(None) => Ok(()),
			Ok(Some(code)) => Err(self.map(send::Error::Stopped(code))),
			Err(err) => Err(self.map(err)),
		})
	}
}

impl std::fmt::Debug for SendStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.state {
			SendState::Open(stream) => stream.fmt(f),
			SendState::Finished(stream) => stream.fmt(f),
			SendState::Reset => f.write_str("Reset"),
		}
	}
}

/// An HTTP/3 error code as a QUIC one; every code HTTP/3 maps to fits.
fn http3_code(code: u64) -> VarInt {
	VarInt::from_u64(code).expect("HTTP/3 codes are varints")
}

/// An incoming stream; its WebTransport header was consumed on accept.
pub struct RecvStream {
	/// Taken by [`stop`](web_transport_trait::poll::RecvStream::stop), since
	/// `web_transport_trait` stops through `&mut self`.
	inner: Option<recv::Stream>,
	web: bool,
}

impl RecvStream {
	fn new(mut inner: recv::Stream, web: bool) -> Self {
		// Same as the send side: the driver stops with a raw 0.
		if web {
			inner.set_drop_stop(http3_code(proto::error_to_http3(0)));
		}
		Self {
			inner: Some(inner),
			web,
		}
	}

	/// Lease the stream's unreleased bytes. See [`recv::Stream::poll_lease`].
	pub fn poll_lease(&mut self, cx: &mut Context<'_>, want: usize) -> Poll<Result<recv::Lease<'_>, Error>> {
		let web = self.web;
		match &mut self.inner {
			Some(inner) => inner.poll_lease(cx, want).map_err(|err| map(web, err.into())),
			None => Poll::Ready(Err(Error::Closed)),
		}
	}

	/// Rank this stream for eviction. See [`recv::Stream::set_priority`].
	pub fn set_priority(&mut self, priority: i64) {
		if let Some(inner) = &mut self.inner {
			inner.set_priority(priority);
		}
	}
}

/// Unmap a stream error's code from the session's code space.
fn map(web: bool, err: Error) -> Error {
	match web {
		true => unmap_err(err),
		false => unmap_legacy(err),
	}
}

impl web_transport_trait::poll::RecvStream for RecvStream {
	type Error = Error;

	fn poll_read(&mut self, cx: &mut Context<'_>, mut dst: &mut [u8]) -> Poll<Result<Option<usize>, Self::Error>> {
		let web = self.web;
		let Some(inner) = &mut self.inner else {
			return Poll::Ready(Err(Error::Closed));
		};
		inner.poll_read(cx, &mut dst).map_err(|err| map(web, err.into()))
	}

	fn poll_read_chunk(&mut self, cx: &mut Context<'_>, max: usize) -> Poll<Result<Option<Bytes>, Self::Error>> {
		let web = self.web;
		let Some(inner) = &mut self.inner else {
			return Poll::Ready(Err(Error::Closed));
		};
		if max == 0 {
			return Poll::Ready(Ok(Some(Bytes::new())));
		}
		inner.poll_read_chunk(cx, max).map_err(|err| map(web, err.into()))
	}

	fn stop(&mut self, code: u32) {
		let code = match self.web {
			true => http3_code(proto::error_to_http3(code)),
			false => VarInt::from_u32(code),
		};
		if let Some(inner) = self.inner.take() {
			inner.stop(code);
		}
	}

	fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
		let web = self.web;
		let Some(inner) = &mut self.inner else {
			return Poll::Ready(Ok(()));
		};
		Poll::Ready(match ready!(inner.poll_closed(cx)) {
			Ok(None) => Ok(()),
			Ok(Some(code)) => Err(map(web, Error::Reset(code.into_inner()))),
			Err(err) => Err(map(web, err.into())),
		})
	}
}

impl std::fmt::Debug for RecvStream {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.inner {
			Some(inner) => inner.fmt(f),
			None => f.write_str("Stopped"),
		}
	}
}

/// Unmap a raw-QUIC stream code that arrived in the HTTP/3 code space.
///
/// Older moq-tokio raw sessions map RESET_STREAM and STOP_SENDING
/// codes as if they were WebTransport. No `u32` application code reaches that
/// range, so a code in it can only be the mapped form. Anything else is the
/// peer's own code and passes through.
fn unmap_legacy(err: Error) -> Error {
	let unmap = |code: u64| proto::error_from_http3(code).map_or(code, u64::from);
	match err {
		Error::Reset(code) => Error::Reset(unmap(code)),
		Error::Stop(code) => Error::Stop(unmap(code)),
		err => err,
	}
}

/// Rewrite HTTP/3-mapped error codes back into WebTransport code space.
///
/// Only a sparse range of HTTP/3 codes names a WebTransport error. Anything
/// else is HTTP/3's own failure rather than a code the peer's application
/// chose, so it becomes [`Error::Http3`]: keeping the `App`/`Reset`/`Stop`
/// variant would advertise it through `session_error()`/`stream_error()` as
/// the very thing this mapping exists to tell it apart from.
fn unmap_err(err: Error) -> Error {
	fn unmap(code: u64) -> Option<u64> {
		proto::error_from_http3(code).map(u64::from)
	}
	match err {
		Error::Reset(code) => match unmap(code) {
			Some(code) => Error::Reset(code),
			None => Error::Http3 {
				code,
				reason: String::new(),
			},
		},
		Error::Stop(code) => match unmap(code) {
			Some(code) => Error::Stop(code),
			None => Error::Http3 {
				code,
				reason: String::new(),
			},
		},
		Error::App { code, reason } => match unmap(code) {
			Some(code) => Error::App { code, reason },
			None => Error::Http3 { code, reason },
		},
		other => other,
	}
}

// ── Incoming stream classification ──────────────────────────────────

/// Incrementally reads one QUIC varint off a stream, never past it.
#[derive(Default)]
struct VarRead {
	buf: [u8; 8],
	have: usize,
}

enum VarPoll {
	Value(u64),
	/// The stream ended (or died) before the varint did.
	End,
}

impl VarRead {
	fn poll(&mut self, cx: &mut Context<'_>, recv: &mut recv::Stream) -> Poll<VarPoll> {
		loop {
			let need = match self.have {
				0 => 1,
				_ => 1usize << (self.buf[0] >> 6),
			};
			if self.have >= need {
				let mut value = u64::from(self.buf[0] & 0x3f);
				for byte in &self.buf[1..need] {
					value = (value << 8) | u64::from(*byte);
				}
				return Poll::Ready(VarPoll::Value(value));
			}
			match ready!(recv.poll_read(cx, &mut &mut self.buf[self.have..need])) {
				Ok(Some(n)) => self.have += n,
				Ok(None) | Err(_) => return Poll::Ready(VarPoll::End),
			}
		}
	}
}

/// Encode one QUIC varint.
fn encode_varint(value: u64, buf: &mut Vec<u8>) {
	if value < 1 << 6 {
		buf.push(value as u8);
	} else if value < 1 << 14 {
		buf.extend_from_slice(&((value as u16) | 0x4000).to_be_bytes());
	} else if value < 1 << 30 {
		buf.extend_from_slice(&((value as u32) | 0x8000_0000).to_be_bytes());
	} else {
		buf.extend_from_slice(&(value | 0xc000_0000_0000_0000).to_be_bytes());
	}
}

/// Decode one QUIC varint off the front of a slice, advancing past it.
fn decode_varint(buf: &mut &[u8]) -> Option<u64> {
	let first = *buf.first()?;
	let len = 1usize << (first >> 6);
	if buf.len() < len {
		return None;
	}
	let mut value = u64::from(first & 0x3f);
	for byte in &buf[1..len] {
		value = (value << 8) | u64::from(*byte);
	}
	*buf = &buf[len..];
	Some(value)
}

/// What an incoming unidirectional stream's header says it is.
enum UniClass {
	/// The H3 control stream, positioned right after its type varint.
	Control,
	/// A QPACK stream: keep it open, never read it.
	Qpack,
	/// A WebTransport stream claiming this session id.
	Web(u64),
	/// Noise (GREASE included); dropping it stops it.
	Unknown,
}

/// An incoming unidirectional stream mid-header.
struct PendingUni {
	recv: recv::Stream,
	typ: VarRead,
	session: VarRead,
	typ_value: Option<u64>,
}

impl PendingUni {
	fn new(recv: recv::Stream) -> Self {
		Self {
			recv,
			typ: VarRead::default(),
			session: VarRead::default(),
			typ_value: None,
		}
	}

	fn poll_classify(&mut self, cx: &mut Context<'_>) -> Poll<UniClass> {
		let typ = match self.typ_value {
			Some(typ) => typ,
			None => match ready!(self.typ.poll(cx, &mut self.recv)) {
				VarPoll::Value(typ) => {
					self.typ_value = Some(typ);
					typ
				}
				VarPoll::End => return Poll::Ready(UniClass::Unknown),
			},
		};
		match typ {
			STREAM_WEBTRANSPORT => match ready!(self.session.poll(cx, &mut self.recv)) {
				VarPoll::Value(session) => Poll::Ready(UniClass::Web(session)),
				VarPoll::End => Poll::Ready(UniClass::Unknown),
			},
			STREAM_CONTROL => Poll::Ready(UniClass::Control),
			STREAM_QPACK_ENCODER | STREAM_QPACK_DECODER => Poll::Ready(UniClass::Qpack),
			_ => {
				tracing::debug!(typ, "ignoring an unknown unidirectional stream");
				Poll::Ready(UniClass::Unknown)
			}
		}
	}
}

/// Progress every pending unidirectional stream; whether any became ready.
fn classify_uni(web: &Rc<Web>, cx: &mut Context<'_>) -> bool {
	// Taken out of the RefCell so classification can push into the other
	// queues without a re-entrant borrow; everything is single-threaded.
	let pending = std::mem::take(&mut web.state.borrow_mut().pending_uni);
	let mut keep = Vec::new();
	let mut progressed = false;

	for mut stream in pending {
		match stream.poll_classify(cx) {
			Poll::Pending => keep.push(stream),
			Poll::Ready(UniClass::Web(session)) if session == web.session_id => {
				web.state.borrow_mut().ready_uni.push_back(stream.recv);
				progressed = true;
			}
			// A second control stream (or a stray QPACK one) is bogus but
			// harmless; holding it beats closing it, which the peer would
			// read as tearing down the H3 connection. Only up to a point: a
			// finished stream returns its credit, so a peer that keeps opening
			// them would grow this vector for as long as it cared to.
			Poll::Ready(UniClass::Control | UniClass::Qpack) => {
				let mut state = web.state.borrow_mut();
				if state.held_recv.len() < HELD_STREAMS {
					state.held_recv.push(stream.recv);
				}
			}
			Poll::Ready(_) => {}
		}
	}

	let mut state = web.state.borrow_mut();
	state.pending_uni.extend(keep);
	progressed
}

/// An incoming bidirectional stream mid-header: frame type, then session id.
struct PendingBi {
	send: send::Stream,
	recv: recv::Stream,
	typ: VarRead,
	session: VarRead,
	typ_value: Option<u64>,
}

impl PendingBi {
	fn new(send: send::Stream, recv: recv::Stream) -> Self {
		Self {
			send,
			recv,
			typ: VarRead::default(),
			session: VarRead::default(),
			typ_value: None,
		}
	}

	/// `Some(session)` for a WebTransport stream, `None` for anything else.
	fn poll_classify(&mut self, cx: &mut Context<'_>) -> Poll<Option<u64>> {
		let typ = match self.typ_value {
			Some(typ) => typ,
			None => match ready!(self.typ.poll(cx, &mut self.recv)) {
				VarPoll::Value(typ) => {
					self.typ_value = Some(typ);
					typ
				}
				VarPoll::End => return Poll::Ready(None),
			},
		};
		if typ != FRAME_WEBTRANSPORT {
			tracing::debug!(typ, "ignoring an unknown bidirectional stream");
			return Poll::Ready(None);
		}
		match ready!(self.session.poll(cx, &mut self.recv)) {
			VarPoll::Value(session) => Poll::Ready(Some(session)),
			VarPoll::End => Poll::Ready(None),
		}
	}
}

/// Progress every pending bidirectional stream; whether any became ready.
fn classify_bi(web: &Rc<Web>, cx: &mut Context<'_>) -> bool {
	let pending = std::mem::take(&mut web.state.borrow_mut().pending_bi);
	let mut keep = Vec::new();
	let mut progressed = false;

	for mut stream in pending {
		match stream.poll_classify(cx) {
			Poll::Pending => keep.push(stream),
			Poll::Ready(Some(session)) if session == web.session_id => {
				web.state.borrow_mut().ready_bi.push_back((stream.send, stream.recv));
				progressed = true;
			}
			Poll::Ready(_) => {}
		}
	}

	let mut state = web.state.borrow_mut();
	state.pending_bi.extend(keep);
	progressed
}

// ── Handshake plumbing ──────────────────────────────────────────────

async fn write_all(send: &mut send::Stream, buf: &[u8]) -> Result<(), Error> {
	Ok(send.write_all(buf).await?)
}

/// Read another chunk into `buf`; `false` once the stream has ended.
///
/// Fails rather than letting `buf` grow past [`MESSAGE_LIMIT`], which is what
/// bounds a peer that declares a huge frame and never finishes it.
async fn read_some(recv: &mut recv::Stream, buf: &mut BytesMut) -> Result<bool, Error> {
	let mut chunk = [0u8; 4096];
	let n = recv.read(&mut &mut chunk[..]).await?;
	match n {
		Some(n) => {
			if buf.len() + n > MESSAGE_LIMIT {
				return Err(Error::Web("an HTTP/3 message exceeded the buffer limit".into()));
			}
			buf.extend_from_slice(&chunk[..n]);
			Ok(true)
		}
		None => Ok(false),
	}
}

/// Read the SETTINGS off a control stream whose type varint was already
/// consumed by classification.
async fn read_settings(recv: &mut recv::Stream) -> Result<proto::Settings, Error> {
	let mut buf = BytesMut::new();
	// The decoder expects the stream to start with its type; re-seed the one
	// byte classification took.
	buf.extend_from_slice(&[STREAM_CONTROL as u8]);
	loop {
		let mut peek: &[u8] = &buf;
		match proto::Settings::decode(&mut peek) {
			Ok(settings) => return Ok(settings),
			Err(proto::SettingsError::UnexpectedEnd) => {}
			Err(err) => return Err(Error::Web(err.to_string())),
		}
		if !read_some(recv, &mut buf).await? {
			return Err(Error::Web("control stream ended before SETTINGS".into()));
		}
	}
}

/// Read the CONNECT request off the first bidirectional stream.
async fn read_connect(recv: &mut recv::Stream) -> Result<proto::ConnectRequest, Error> {
	let mut buf = BytesMut::new();
	loop {
		let mut peek: &[u8] = &buf;
		match proto::ConnectRequest::decode(&mut peek) {
			Ok(request) => return Ok(request),
			Err(proto::ConnectError::UnexpectedEnd) => {}
			Err(err) => return Err(Error::Web(err.to_string())),
		}
		if !read_some(recv, &mut buf).await? {
			return Err(Error::Web("stream ended before the CONNECT request".into()));
		}
	}
}

// ── Session close capsules ──────────────────────────────────────────

/// Read the CONNECT stream until it ends, surfacing the peer's
/// `CloseWebTransportSession` capsule (carried in HTTP/3 DATA frames) as the
/// session's error, then close the connection.
async fn read_capsules(web: Rc<Web>, conn: Connection, mut recv: recv::Stream) {
	let mut capsules = Capsules::default();
	let capsule = loop {
		match capsules.take() {
			Ok(Some(proto::Capsule::CloseWebTransportSession { code, reason })) => {
				break Some((code, reason));
			}
			// GREASE and anything else defined later: skip it and keep
			// reading, since the close capsule may still be behind it.
			Ok(Some(_)) => continue,
			Ok(None) => {}
			Err(err) => {
				tracing::debug!(%err, "failed to parse a capsule on the CONNECT stream");
				break None;
			}
		}
		match read_some(&mut recv, &mut capsules.frames).await {
			Ok(true) => {}
			// A clean FIN without a capsule, or the connection died under
			// the stream; either way the session is over.
			Ok(false) | Err(_) => break None,
		}
	};

	match capsule {
		Some((code, reason)) => {
			web.closed.borrow_mut().get_or_insert(Error::App {
				code: u64::from(code),
				reason: reason.clone(),
			});
			conn.close_code(proto::error_to_http3(code), &reason);
		}
		// The CONNECT stream ending closes the session with no error.
		None => conn.close_code(proto::error_to_http3(0), ""),
	}
}

/// The CONNECT stream's capsule reader.
///
/// HTTP/3 framing and the Capsule Protocol are independent layers: a capsule
/// may span DATA frames and one DATA frame may carry several, so the payloads
/// are concatenated and parsed as one continuous byte stream rather than
/// frame by frame.
#[derive(Default)]
struct Capsules {
	/// Stream bytes whose HTTP/3 framing has not been split off yet.
	frames: BytesMut,
	/// The DATA payloads, concatenated: the capsule stream itself.
	body: BytesMut,
}

impl Capsules {
	/// Take the next whole capsule, or `None` until one has fully arrived.
	fn take(&mut self) -> Result<Option<proto::Capsule>, Error> {
		self.demux()?;

		let mut peek: &[u8] = &self.body;
		match proto::Capsule::decode(&mut peek) {
			Ok(capsule) => {
				let consumed = self.body.len() - peek.len();
				self.body.advance(consumed);
				Ok(Some(capsule))
			}
			// A short header and a short body are both just "not yet".
			Err(proto::CapsuleError::UnexpectedEnd | proto::CapsuleError::VarInt(_)) => Ok(None),
			Err(err) => Err(Error::Web(err.to_string())),
		}
	}

	/// Move every whole DATA payload into [`body`](Self::body), skipping other
	/// frame types entirely.
	fn demux(&mut self) -> Result<(), Error> {
		loop {
			let mut peek: &[u8] = &self.frames;
			let Some(typ) = decode_varint(&mut peek) else {
				return Ok(());
			};
			let Some(len) = decode_varint(&mut peek) else {
				return Ok(());
			};
			let len = usize::try_from(len).map_err(|_| Error::Web("oversized HTTP/3 frame".into()))?;
			if peek.len() < len {
				return Ok(());
			}
			let header = self.frames.len() - peek.len();

			if typ == FRAME_DATA {
				// Bounded like the frame buffer: a capsule nothing ever
				// completes must not grow here either.
				if self.body.len() + len > MESSAGE_LIMIT {
					return Err(Error::Web("a capsule exceeded the buffer limit".into()));
				}
				self.body.extend_from_slice(&peek[..len]);
			}
			self.frames.advance(header + len);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A code with a WebTransport meaning comes back out as itself; one
	/// without is HTTP/3's own failure, and must stop advertising itself as an
	/// application error through the trait accessors.
	#[test]
	fn an_unmappable_code_is_not_an_application_error() {
		use web_transport_trait::Error as _;

		let app = unmap_err(Error::App {
			code: proto::error_to_http3(7),
			reason: "seven".into(),
		});
		assert!(
			matches!(&app, Error::App { code: 7, reason } if reason == "seven"),
			"got {app:?}"
		);
		assert_eq!(app.session_error(), Some((7, "seven".to_string())));

		// H3_NO_ERROR: HTTP/3's, and no WebTransport code at all.
		let h3 = unmap_err(Error::App {
			code: 0x100,
			reason: "done".into(),
		});
		assert!(
			matches!(&h3, Error::Http3 { code: 0x100, reason } if reason == "done"),
			"got {h3:?}"
		);
		assert_eq!(h3.session_error(), None, "not a code the peer's application chose");

		let reset = unmap_err(Error::Reset(0x100));
		assert!(matches!(reset, Error::Http3 { code: 0x100, .. }), "got {reset:?}");
		assert_eq!(reset.stream_error(), None, "not a MoQ stream code");
		assert!(matches!(
			unmap_err(Error::Stop(proto::error_to_http3(3))),
			Error::Stop(3)
		));
	}

	/// A raw session reads a plain stream code as is, and an older raw peer's
	/// HTTP/3-mapped one as the code it stands for. Session closes are left alone.
	#[test]
	fn a_raw_session_unmaps_legacy_stream_codes() {
		assert!(matches!(unmap_legacy(Error::Reset(5)), Error::Reset(5)));
		assert!(matches!(unmap_legacy(Error::Stop(5)), Error::Stop(5)));
		assert!(matches!(
			unmap_legacy(Error::Reset(proto::error_to_http3(5))),
			Error::Reset(5)
		));
		assert!(matches!(
			unmap_legacy(Error::Stop(proto::error_to_http3(5))),
			Error::Stop(5)
		));

		let code = proto::error_to_http3(5);
		assert!(matches!(
			unmap_legacy(Error::App {
				code,
				reason: String::new()
			}),
			Error::App { code: c, .. } if c == code
		));
	}

	/// Wrap `payload` in an HTTP/3 frame of type `typ`.
	fn frame(typ: u64, payload: &[u8]) -> Vec<u8> {
		let mut buf = Vec::new();
		encode_varint(typ, &mut buf);
		encode_varint(payload.len() as u64, &mut buf);
		buf.extend_from_slice(payload);
		buf
	}

	fn close_capsule(code: u32, reason: &str) -> Vec<u8> {
		let mut buf = Vec::new();
		proto::Capsule::CloseWebTransportSession {
			code,
			reason: reason.to_string(),
		}
		.encode(&mut buf);
		buf
	}

	/// The Capsule Protocol runs across DATA frames, so a capsule cut in half
	/// by the framing still has to come out whole.
	#[test]
	fn a_capsule_spans_data_frames() {
		let capsule = close_capsule(42, "split");
		let (head, tail) = capsule.split_at(capsule.len() / 2);

		let mut capsules = Capsules::default();
		capsules.frames.extend_from_slice(&frame(FRAME_DATA, head));
		assert!(capsules.take().expect("parse").is_none(), "half a capsule is not one");

		capsules.frames.extend_from_slice(&frame(FRAME_DATA, tail));
		let capsule = capsules.take().expect("parse").expect("the second half completes it");
		assert_eq!(
			capsule,
			proto::Capsule::CloseWebTransportSession {
				code: 42,
				reason: "split".to_string()
			}
		);
	}

	/// One DATA frame may carry several capsules, and reading the first must
	/// not discard the rest.
	#[test]
	fn one_frame_carries_several_capsules() {
		let mut payload = close_capsule(1, "first");
		payload.extend_from_slice(&close_capsule(2, "second"));

		let mut capsules = Capsules::default();
		capsules.frames.extend_from_slice(&frame(FRAME_DATA, &payload));

		for (code, reason) in [(1, "first"), (2, "second")] {
			let capsule = capsules.take().expect("parse").expect("a whole capsule");
			assert_eq!(
				capsule,
				proto::Capsule::CloseWebTransportSession {
					code,
					reason: reason.to_string()
				}
			);
		}
		assert!(capsules.take().expect("parse").is_none(), "only two were written");
	}

	/// Frames that are not DATA carry no capsules; skipping one must not
	/// disturb the capsule stream around it.
	#[test]
	fn a_non_data_frame_is_skipped() {
		let capsule = close_capsule(7, "after");
		let (head, tail) = capsule.split_at(1);

		let mut capsules = Capsules::default();
		capsules.frames.extend_from_slice(&frame(FRAME_DATA, head));
		// 0x07 is GOAWAY: legal on the stream, and not a capsule carrier.
		capsules.frames.extend_from_slice(&frame(0x07, b"\x00"));
		capsules.frames.extend_from_slice(&frame(FRAME_DATA, tail));

		let capsule = capsules.take().expect("parse").expect("a whole capsule");
		assert_eq!(
			capsule,
			proto::Capsule::CloseWebTransportSession {
				code: 7,
				reason: "after".to_string()
			}
		);
	}

	/// A peer that declares a capsule payload it never sends must not grow the
	/// buffer without end.
	#[test]
	fn a_capsule_stream_is_bounded() {
		// A close capsule whose declared length never arrives. 65536 is the
		// largest the decoder entertains, so it keeps asking for more rather
		// than refusing the length outright.
		let mut header = Vec::new();
		encode_varint(0x2843, &mut header);
		encode_varint(65536, &mut header);

		let mut capsules = Capsules::default();
		capsules.frames.extend_from_slice(&frame(FRAME_DATA, &header));

		let chunk = vec![0u8; 8 * 1024];
		let err = loop {
			match capsules.take() {
				Ok(None) => {}
				Ok(Some(capsule)) => panic!("the payload never arrived, got {capsule:?}"),
				Err(err) => break err,
			}
			capsules.frames.extend_from_slice(&frame(FRAME_DATA, &chunk));
		};
		assert!(matches!(err, Error::Web(_)), "refused with {err}");
		assert!(capsules.body.len() <= MESSAGE_LIMIT, "the buffer stayed bounded");
	}
}
