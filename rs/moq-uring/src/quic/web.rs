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

use futures::{StreamExt, stream::FuturesUnordered};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, ready};

use bytes::{Buf, Bytes, BytesMut};
/// HTTP types used when accepting or refusing a session.
pub use http;
/// The HTTP/3 and WebTransport wire types used by this API.
pub use web_transport_proto as proto;
/// The HTTP/3 ALPN used by WebTransport.
pub const ALPN: &str = "h3";

use super::{Connection, Error, VarInt, recv as driver_recv, send as driver_send};

/// Incoming WebTransport streams and their errors.
pub mod recv;
/// Outgoing WebTransport streams and their errors.
pub mod send;
use self::{recv::Stream as RecvStream, send::Stream as SendStream};

/// An error establishing or using a WebTransport session on this worker.
pub use super::Error as SessionError;

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
	send: driver_send::Stream,
	recv: driver_recv::Stream,
	/// The peer's control and QPACK streams, held open for the session's
	/// life: closing them reads as tearing the H3 connection down.
	held: [Option<driver_recv::Stream>; 3],
	/// Our control stream, same deal.
	control: driver_send::Stream,
	/// WebTransport streams a pipelining peer opened before its CONNECT was
	/// answered, headers consumed, keyed by the session id they claimed.
	early: Vec<(u64, driver_recv::Stream)>,
	/// Streams that arrived during the handshake and are still mid-header;
	/// the session goes on classifying them.
	pending: FuturesUnordered<PendingUni>,
}

/// Closes a connection whose handshake was abandoned, disarmed once the peer
/// has an answer.
///
/// Dropping every handle would close QUIC with code 0, which is no HTTP/3
/// error, and any handle the caller still holds would keep the connection open
/// with no answer coming. The peer picks the path it asks for and the
/// subprotocols it offers, so it decides which of [`Request::respond`]'s
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

	async fn handshake(conn: Connection) -> Result<Self, Error> {
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
		let mut pending: FuturesUnordered<PendingUni> = FuturesUnordered::new();
		let mut held = [None, None, None];
		let mut early = Vec::new();
		let mut peer_control = None;
		kio::wait(|waiter| {
			let mut cx = waiter.context();
			loop {
				while let Poll::Ready(recv) = conn.poll_accept_uni(waiter) {
					pending.push(PendingUni::new(recv?));
				}

				// Each mid-header stream parks the caller in its own stream's
				// waiters, so progress on any of them re-polls us.
				let mut progressed = false;
				while let Poll::Ready(Some((class, recv))) = pending.poll_next_unpin(&mut cx) {
					progressed = true;
					match class {
						UniClass::Control => {
							peer_control = Some(recv);
							return Poll::Ready(Ok::<(), Error>(()));
						}
						UniClass::QpackEncoder => held[1] = Some(recv),
						UniClass::QpackDecoder => held[2] = Some(recv),
						UniClass::Web(session) => early.push((session, recv)),
						UniClass::Unknown => {}
					}
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
		held[0] = Some(peer_control);

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

	/// Reply to the CONNECT request with a successful WebTransport response.
	pub async fn respond(mut self, response: impl Into<proto::ConnectResponse>) -> Result<Session, Error> {
		let response = response.into();
		if response.status != http::StatusCode::OK {
			return Err(Error::Web("respond requires OK; use reject for other statuses".into()));
		}
		if let Some(protocol) = &response.protocol
			&& !self.request.protocols.contains(protocol)
		{
			return Err(Error::Web(format!("subprotocol {protocol:?} was not offered")));
		}
		let mut buf = Vec::new();
		response.encode(&mut buf).map_err(|err| Error::Web(err.to_string()))?;
		write_all(&mut self.send, &buf).await?;
		self.guard.disarm();
		Ok(Session::establish(self, response))
	}

	/// The QUIC connection carrying this handshake.
	pub fn connection(&self) -> &Connection {
		&self.conn
	}

	/// Accept with a `200` and no subprotocol.
	pub async fn ok(self) -> Result<Session, Error> {
		self.respond(proto::ConnectResponse::OK).await
	}

	/// Refuse with a 4xx or 5xx status code, ending the handshake.
	///
	/// Returns once the peer has the response, or after a grace period if it
	/// never acknowledges one, and closes the connection deliberately. The
	/// HTTP/3 critical streams (the peer's control and QPACK streams, and
	/// ours) stay open until then: RFC 9114 makes closing one a connection
	/// error, so tearing them down here would show the peer an H3 failure
	/// instead of the status it was sent.
	pub async fn reject(mut self, status: http::StatusCode) -> Result<(), Error> {
		if !status.is_client_error() && !status.is_server_error() {
			return Err(Error::Web("reject requires a 4xx or 5xx status".into()));
		}
		let response = proto::ConnectResponse::new(status);
		let mut buf = Vec::new();
		response.encode(&mut buf).map_err(|err| Error::Web(err.to_string()))?;
		write_all(&mut self.send, &buf).await?;
		let send = self.send.finish();

		// The guard stays armed across the wait below. Cancelling this future
		// mid-grace would otherwise skip the deliberate close and leak the
		// connection, which is the very thing the guard is here to prevent.
		let mut deadline = self.conn.owner().after(CLOSE_GRACE);
		kio::wait(|waiter| {
			if send.poll_closed(waiter).is_ready() {
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

impl std::ops::Deref for Request {
	type Target = proto::ConnectRequest;
	fn deref(&self) -> &Self::Target {
		&self.request
	}
}

/// Safe to log: [`proto::ConnectRequest`]'s `Debug` redacts the URL's
/// userinfo, query, and fragment and every header value but `origin` and
/// `user-agent`, so a `?jwt=` or `Authorization` credential stays out.
impl std::fmt::Debug for Request {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Request").field("connect", &self.request).finish()
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
	state: kio::Bridge<State>,
	/// The session's terminal error, in WebTransport code space: a received
	/// `CloseWebTransportSession` capsule, or the local [`Session::close`].
	/// First writer wins.
	closed: RefCell<Option<Error>>,
}

struct State {
	uni_park: kio::Park,
	bi_park: kio::Park,
	/// Incoming streams whose headers are still being read.
	pending_uni: FuturesUnordered<PendingUni>,
	pending_bi: FuturesUnordered<PendingBi>,
	/// Classified application streams nobody has accepted yet.
	ready_uni: VecDeque<driver_recv::Stream>,
	ready_bi: VecDeque<(driver_send::Stream, driver_recv::Stream)>,
	/// Streams held open, never read: control and QPACK, ours and theirs.
	held_recv: [Option<driver_recv::Stream>; 3],
	_control_send: Option<driver_send::Stream>,
	/// The CONNECT stream's send half; taken exactly once by the close path.
	connect_send: Option<driver_send::Stream>,
}

/// A MoQ transport over the worker: raw QUIC, or a WebTransport session.
///
/// [`Request::respond`] builds the web
/// flavor; [`Session::raw`] wraps a raw-QUIC [`Connection`] with the layering
/// disabled. Clones share the session, which closes with code 0 once its last
/// clone and stream are gone.
pub struct Session {
	opening: Rc<kio::Bridge<Opening>>,
	handles: Rc<Handles>,
	conn: Connection,
	web: Option<Rc<Web>>,
	/// What [`protocol`](Self::protocol)
	/// reports: the selected subprotocol in web mode, the ALPN in raw mode.
	alpn: Option<String>,
	request: Option<proto::ConnectRequest>,
	response: Option<proto::ConnectResponse>,
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

	/// The receive windows now in force: the connection's, and the most one stream holds.
	pub(crate) fn receive_window(&self) -> moq_net::transport::ReceiveWindow {
		self.conn.receive_window()
	}

	/// Open a send group at `priority`, for streams to join with
	/// [`SendStream::set_group`]. The HTTP/3 streams stay outside every
	/// group, so they go first.
	pub fn send_group(&self, priority: i64) -> driver_send::Group {
		self.conn.send_group(priority)
	}

	/// Send a datagram scheduled by `options`, like one of its group's streams.
	///
	/// See [`Connection::send_datagram`].
	pub fn send_datagram(&self, payload: impl Buf, options: driver_send::Options<'_>) -> Result<(), Error> {
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
		let alpn = conn.protocol().map(str::to_owned);
		Self {
			opening: Default::default(),
			handles: Handles::new(&conn, None),
			conn,
			web: None,
			alpn,
			request: None,
			response: None,
		}
	}

	/// Change the underlying connection's flow-control limits; see
	/// [`Connection::set_limits`].
	pub fn set_limits(&self, limits: super::Limits) -> Result<(), super::LimitsError> {
		self.conn.set_limits(limits)
	}

	/// Assemble the web flavor and spawn its capsule reader.
	fn establish(request: Request, response: proto::ConnectResponse) -> Self {
		let Request {
			conn,
			guard: _,
			request,
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
			state: kio::Bridge::new(State {
				uni_park: Default::default(),
				bi_park: Default::default(),
				// Still mid-header when the control stream turned up; the
				// session finishes classifying them, so a pipelined
				// WebTransport stream is not lost to the handshake ending.
				pending_uni: pending,
				pending_bi: FuturesUnordered::new(),
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
			opening: Default::default(),
			handles: Handles::new(&conn, Some(&web)),
			conn,
			web: Some(web),
			alpn: None,
			request: Some(request),
			response: Some(response),
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
			opening: self.opening.clone(),
			handles: self.handles.clone(),
			conn: self.conn.clone(),
			web: self.web.clone(),
			alpn: self.alpn.clone(),
			request: self.request.clone(),
			response: self.response.clone(),
		}
	}
}

impl std::fmt::Debug for Session {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Session")
			.field("web", &self.web.is_some())
			.field("protocol", &self.protocol())
			.finish()
	}
}

#[derive(Default)]
struct Opening {
	// Driver wakes must go through the bridge, so they cannot reenter its held lock.
	park: kio::Park,
	uni: Option<(SendStream, Bytes)>,
	bi: Option<(SendStream, RecvStream, Bytes)>,
}

impl Session {
	/// Poll for a unidirectional stream after its session header.
	pub fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>> {
		let Some(web) = &self.web else {
			return self
				.conn
				.poll_accept_uni(waiter)
				.map_ok(|recv| RecvStream::new(recv, &self.handles));
		};
		web.state.poll(waiter, |state, cx| {
			loop {
				if let Some(recv) = state.ready_uni.pop_front() {
					return Poll::Ready(Ok(RecvStream::new(recv, &self.handles)));
				}
				let ended = loop {
					match self.conn.poll_accept_uni(state.uni_park.hold(cx)) {
						Poll::Ready(Ok(recv)) => state.pending_uni.push(PendingUni::new(recv)),
						Poll::Ready(Err(err)) => break Some(err),
						Poll::Pending => break None,
					}
				};
				if !classify_uni(web.session_id, state, cx) {
					if state.pending_uni.is_empty()
						&& let Some(err) = ended
					{
						return Poll::Ready(Err(unmap_err(err)));
					}
					return Poll::Pending;
				}
			}
		})
	}

	/// Poll for a bidirectional stream after its session header.
	pub fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		let Some(web) = &self.web else {
			return self.conn.poll_accept_bi(waiter).map_ok(|(send, recv)| {
				(
					SendStream::new(send, &self.handles, None),
					RecvStream::new(recv, &self.handles),
				)
			});
		};
		web.state.poll(waiter, |state, cx| {
			loop {
				if let Some((send, recv)) = state.ready_bi.pop_front() {
					return Poll::Ready(Ok((
						SendStream::new(send, &self.handles, None),
						RecvStream::new(recv, &self.handles),
					)));
				}
				let ended = loop {
					match self.conn.poll_accept_bi(state.bi_park.hold(cx)) {
						Poll::Ready(Ok((send, recv))) => state.pending_bi.push(PendingBi::new(send, recv)),
						Poll::Ready(Err(err)) => break Some(err),
						Poll::Pending => break None,
					}
				};
				if !classify_bi(web.session_id, state, cx) {
					if state.pending_bi.is_empty()
						&& let Some(err) = ended
					{
						return Poll::Ready(Err(unmap_err(err)));
					}
					return Poll::Pending;
				}
			}
		})
	}

	/// Poll to open a unidirectional stream scheduled by `options`.
	pub fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: driver_send::Options<'_>,
	) -> Poll<Result<SendStream, Error>> {
		self.opening.poll(waiter, |opening, cx| {
			if opening.uni.is_none() {
				let inner = ready!(self.conn.poll_open_uni(
					opening.park.hold(cx),
					driver_send::Options {
						group: None,
						priority: i64::MAX
					}
				))
				.map_err(|err| self.map_err(err))?;
				let header = self.web.as_ref().map(|web| web.header_uni.clone()).unwrap_or_default();
				opening.uni = Some((SendStream::new(inner, &self.handles, None), header));
			}
			let (send, header) = opening.uni.as_mut().unwrap();
			let result = ready!(send.poll_header(opening.park.hold(cx), header)).map_err(Error::from);
			let (mut send, _) = opening.uni.take().unwrap();
			result?;
			send.set_priority(options.priority);
			send.set_group(options.group);
			Poll::Ready(Ok(send))
		})
	}

	/// Poll to open a bidirectional stream scheduled by `options`.
	pub fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: driver_send::Options<'_>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		self.opening.poll(waiter, |opening, cx| {
			if opening.bi.is_none() {
				let (mut send, recv) = ready!(self.conn.poll_open_bi(
					opening.park.hold(cx),
					driver_send::Options {
						group: None,
						priority: i64::MAX
					}
				))
				.map_err(|e| self.map_err(e))?;
				send.set_priority(i64::MAX);
				let header = self.web.as_ref().map(|web| web.header_bi.clone()).unwrap_or_default();
				opening.bi = Some((
					SendStream::new(send, &self.handles, None),
					RecvStream::new(recv, &self.handles),
					header,
				));
			}
			let (send, _, header) = opening.bi.as_mut().unwrap();
			let result = ready!(send.poll_header(opening.park.hold(cx), header)).map_err(Error::from);
			let (mut send, recv, _) = opening.bi.take().unwrap();
			result?;
			send.set_priority(options.priority);
			send.set_group(options.group);
			Poll::Ready(Ok((send, recv)))
		})
	}

	/// Poll for the next datagram after its session header.
	///
	/// A datagram that does not name this session, such as another session's on the same
	/// connection, is dropped: datagrams are unreliable, so a stray one must not end the session.
	pub fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		loop {
			let datagram = ready!(self.conn.poll_recv_datagram(waiter)).map_err(|e| self.map_err(e))?;
			let Some(web) = &self.web else {
				return Poll::Ready(Ok(datagram));
			};
			let mut peek: &[u8] = &datagram;
			match decode_varint(&mut peek) {
				Some(id) if id == web.session_id => {
					return Poll::Ready(Ok(datagram.slice(datagram.len() - peek.len()..)));
				}
				_ => tracing::debug!("dropping a datagram for an unknown session"),
			}
		}
	}

	/// The maximum datagram payload, excluding its session header.
	pub fn max_datagram_size(&self) -> usize {
		self.conn
			.max_datagram_size()
			.unwrap_or(0)
			.saturating_sub(self.web.as_ref().map_or(0, |web| web.header_datagram.len()))
	}
	/// The CONNECT request, or `None` for raw QUIC.
	pub fn request(&self) -> Option<&proto::ConnectRequest> {
		self.request.as_ref()
	}
	/// The CONNECT response, or `None` for raw QUIC.
	pub fn response(&self) -> Option<&proto::ConnectResponse> {
		self.response.as_ref()
	}
	/// The negotiated WebTransport subprotocol, or raw QUIC ALPN.
	pub fn protocol(&self) -> Option<&str> {
		self.response
			.as_ref()
			.and_then(|response| response.protocol.as_deref())
			.or(self.alpn.as_deref())
	}
	/// Close the session with an application code and reason.
	pub fn close(&self, code: u32, reason: &str) {
		close(&self.conn, self.web.as_ref(), code, reason);
	}
	/// Poll until the session's connection has closed.
	pub fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		let err = ready!(self.conn.poll_closed(waiter));
		Poll::Ready(
			self.web
				.as_ref()
				.and_then(|web| web.closed.borrow().clone())
				.unwrap_or_else(|| self.map_err(err)),
		)
	}
	/// Return connection-level QUIC statistics.
	pub fn stats(&self) -> moq_quic::connection::Stats {
		self.conn.stats()
	}
	/// The QUIC connection carrying this session.
	pub fn connection(&self) -> &Connection {
		&self.conn
	}
	/// Accept the next unidirectional stream after its session header.
	pub async fn accept_uni(&self) -> Result<RecvStream, Error> {
		kio::wait(|w| self.poll_accept_uni(w)).await
	}
	/// Accept the next bidirectional stream after its session header.
	pub async fn accept_bi(&self) -> Result<(SendStream, RecvStream), Error> {
		kio::wait(|w| self.poll_accept_bi(w)).await
	}
	/// Open a unidirectional stream scheduled by `options`.
	pub async fn open_uni(&self, options: driver_send::Options<'_>) -> Result<SendStream, Error> {
		kio::wait(|w| self.poll_open_uni(w, options)).await
	}
	/// Open a bidirectional stream scheduled by `options`.
	pub async fn open_bi(&self, options: driver_send::Options<'_>) -> Result<(SendStream, RecvStream), Error> {
		kio::wait(|w| self.poll_open_bi(w, options)).await
	}
	/// Receive a datagram with its session header removed.
	pub async fn recv_datagram(&self) -> Result<Bytes, Error> {
		kio::wait(|w| self.poll_recv_datagram(w)).await
	}
	/// Wait until the session's connection has closed.
	pub async fn closed(&self) -> Error {
		kio::wait(|w| self.poll_closed(w)).await
	}
}

impl moq_net::transport::poll::Session for Session {
	type SendStream = SendStream;
	type RecvStream = RecvStream;
	type Error = Error;
	fn poll_accept_uni(&self, waiter: &kio::Waiter) -> Poll<Result<RecvStream, Error>> {
		Self::poll_accept_uni(self, waiter)
	}
	fn poll_accept_bi(&self, waiter: &kio::Waiter) -> Poll<Result<(SendStream, RecvStream), Error>> {
		Self::poll_accept_bi(self, waiter)
	}
	fn poll_open_uni(
		&self,
		waiter: &kio::Waiter,
		options: moq_net::transport::poll::SendOptions<'_, driver_send::Group>,
	) -> Poll<Result<SendStream, Error>> {
		Self::poll_open_uni(
			self,
			waiter,
			driver_send::Options {
				group: options.group,
				priority: options.priority,
			},
		)
	}
	fn poll_open_bi(
		&self,
		waiter: &kio::Waiter,
		options: moq_net::transport::poll::SendOptions<'_, driver_send::Group>,
	) -> Poll<Result<(SendStream, RecvStream), Error>> {
		Self::poll_open_bi(
			self,
			waiter,
			driver_send::Options {
				group: options.group,
				priority: options.priority,
			},
		)
	}
	fn send_group(&self, priority: i64) -> Option<driver_send::Group> {
		Some(Self::send_group(self, priority))
	}
	fn send_datagram(
		&self,
		payload: Bytes,
		options: moq_net::transport::poll::SendOptions<'_, driver_send::Group>,
	) -> Result<(), Error> {
		Self::send_datagram(
			self,
			payload,
			driver_send::Options {
				group: options.group,
				priority: options.priority,
			},
		)
	}
	fn poll_recv_datagram(&self, waiter: &kio::Waiter) -> Poll<Result<Bytes, Error>> {
		Self::poll_recv_datagram(self, waiter)
	}
	fn max_datagram_size(&self) -> usize {
		Self::max_datagram_size(self)
	}
	fn protocol(&self) -> Option<&str> {
		Self::protocol(self)
	}
	fn address(&self) -> moq_net::transport::Address {
		if self.web.is_some() {
			moq_net::transport::Address::Uri
		} else {
			moq_net::transport::Address::Setup
		}
	}
	fn close(&self, code: u32, reason: &str) {
		Self::close(self, code, reason);
	}
	fn poll_closed(&self, waiter: &kio::Waiter) -> Poll<Error> {
		Self::poll_closed(self, waiter)
	}
	fn metrics(&self) -> impl moq_net::transport::Metrics {
		self.conn.transport_stats()
	}
	fn wire(&self) -> Option<moq_net::transport::Wire> {
		Some(Self::wire(self))
	}
	fn probe(&self) -> Option<moq_net::transport::Probe> {
		Self::probe(self)
	}
	fn receive_window(&self) -> Option<moq_net::transport::ReceiveWindow> {
		Some(Self::receive_window(self))
	}
}

/// Close the session: in web mode with a `CloseWebTransportSession` capsule
/// ahead of the connection close, which carries the code and reason to a
/// browser.
fn close(conn: &Connection, web: Option<&Rc<Web>>, code: u32, reason: &str) {
	let Some(web) = web else {
		return conn.close_code(u64::from(code), reason);
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

	let connect_send = web.state.lock().connect_send.take();
	let http3 = proto::error_to_http3(code);
	let Some(send) = connect_send else {
		conn.close_code(http3, reason);
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
	let mut deadline = conn.owner().after(CLOSE_GRACE);
	let reason = reason.to_string();
	let owner = conn.owner().clone();
	let conn = conn.clone();
	// The caller may drop every handle right after this, so the task holds
	// the H3 control and QPACK streams itself: RFC 9114 makes closing one a
	// connection error, which a browser reports instead of the capsule.
	let web = web.clone();
	owner.spawn(async move {
		let mut frame = Bytes::from(frame);
		// Taken to finish once the frame is all written, and kept until the close.
		let mut send = Some(send);
		let mut finished = None;
		kio::wait(|waiter| {
			// Stop writing once the connection is gone; the deadline is the
			// only other thing that ends this.
			if conn.poll_closed(waiter).is_ready() {
				return Poll::Ready(());
			}
			if let Some(stream) = &mut send {
				while frame.has_remaining() {
					match stream.poll_write_buf(waiter, &mut frame) {
						Poll::Ready(Ok(_)) => {}
						// The stream is unusable, so the connection close below
						// is all the peer is going to get.
						Poll::Ready(Err(_)) => return Poll::Ready(()),
						Poll::Pending => break,
					}
				}
				if !frame.has_remaining() {
					finished = send.take().map(driver_send::Stream::finish);
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

/// What every session clone and stream it handed out shares: the last to drop
/// closes the session.
///
/// Background tasks (the capsule reader, a graceful close) hold the connection
/// and its HTTP/3 streams but never this, so they cannot keep a session nobody
/// can use open.
struct Handles {
	conn: Connection,
	web: Option<Rc<Web>>,
}

impl Handles {
	fn new(conn: &Connection, web: Option<&Rc<Web>>) -> Rc<Self> {
		Rc::new(Self {
			conn: conn.clone(),
			web: web.cloned(),
		})
	}
}

impl Handles {
	fn encode_stream_code(&self, code: u32) -> VarInt {
		if self.web.is_some() {
			http3_code(proto::error_to_http3(code))
		} else {
			code.into()
		}
	}
	fn decode_stream_code(&self, code: VarInt) -> Option<u32> {
		decode_stream_code(self.web.is_none(), code)
	}

	fn session_gone(&self, code: VarInt) -> Option<Error> {
		self.web
			.as_ref()
			.filter(|_| code.into_inner() == 0x170d7b68)
			.map(|web| {
				web.closed
					.borrow()
					.clone()
					.unwrap_or_else(|| Error::Web("session gone".into()))
			})
	}
	fn map(&self, err: Error) -> Error {
		if let Some(web) = &self.web {
			web.closed.borrow().clone().unwrap_or_else(|| unmap_err(err))
		} else {
			err
		}
	}
}

impl Drop for Handles {
	fn drop(&mut self) {
		close(&self.conn, self.web.as_ref(), 0, "dropped");
	}
}

/// An HTTP/3 code as a QUIC varint.
fn http3_code(code: u64) -> VarInt {
	VarInt::from_u64(code).expect("HTTP/3 codes are varints")
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
	park: kio::Park,
	buf: [u8; 8],
	have: usize,
}

enum VarPoll {
	Value(u64),
	/// The stream ended (or died) before the varint did.
	End,
}

impl VarRead {
	fn poll(&mut self, cx: &mut Context<'_>, recv: &mut driver_recv::Stream) -> Poll<VarPoll> {
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
			match ready!(recv.poll_read_buf(self.park.hold(cx), &mut &mut self.buf[self.have..need])) {
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
	QpackEncoder,
	QpackDecoder,
	/// A WebTransport stream claiming this session id.
	Web(u64),
	/// Noise (GREASE included); dropping it stops it.
	Unknown,
}

/// An incoming unidirectional stream mid-header.
struct PendingUni {
	recv: Option<driver_recv::Stream>,
	typ: VarRead,
	session: VarRead,
	typ_value: Option<u64>,
}

impl PendingUni {
	fn new(recv: driver_recv::Stream) -> Self {
		Self {
			recv: Some(recv),
			typ: VarRead::default(),
			session: VarRead::default(),
			typ_value: None,
		}
	}

	fn poll_classify(&mut self, cx: &mut Context<'_>) -> Poll<UniClass> {
		let typ = match self.typ_value {
			Some(typ) => typ,
			None => match ready!(self.typ.poll(cx, self.recv.as_mut().expect("not yet classified"))) {
				VarPoll::Value(typ) => {
					self.typ_value = Some(typ);
					typ
				}
				VarPoll::End => return Poll::Ready(UniClass::Unknown),
			},
		};
		match typ {
			STREAM_WEBTRANSPORT => match ready!(self.session.poll(cx, self.recv.as_mut().expect("not yet classified")))
			{
				VarPoll::Value(session) => Poll::Ready(UniClass::Web(session)),
				VarPoll::End => Poll::Ready(UniClass::Unknown),
			},
			STREAM_CONTROL => Poll::Ready(UniClass::Control),
			STREAM_QPACK_ENCODER => Poll::Ready(UniClass::QpackEncoder),
			STREAM_QPACK_DECODER => Poll::Ready(UniClass::QpackDecoder),
			_ => {
				tracing::debug!(typ, "ignoring an unknown unidirectional stream");
				Poll::Ready(UniClass::Unknown)
			}
		}
	}
}

impl Future for PendingUni {
	type Output = (UniClass, driver_recv::Stream);
	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let class = ready!(self.poll_classify(cx));
		Poll::Ready((class, self.recv.take().expect("not yet classified")))
	}
}

/// Poll only newly arrived or woken headers.
fn classify_uni(session_id: u64, state: &mut State, cx: &mut Context<'_>) -> bool {
	let mut progressed = false;
	while let Poll::Ready(Some((class, recv))) = state.pending_uni.poll_next_unpin(cx) {
		match class {
			UniClass::Web(session) if session == session_id => {
				state.ready_uni.push_back(recv);
				progressed = true;
			}
			UniClass::Control => {
				state.held_recv[0].get_or_insert(recv);
			}
			UniClass::QpackEncoder => {
				state.held_recv[1].get_or_insert(recv);
			}
			UniClass::QpackDecoder => {
				state.held_recv[2].get_or_insert(recv);
			}
			_ => {}
		}
	}
	progressed
}

/// An incoming bidirectional stream mid-header: frame type, then session id.
struct PendingBi {
	send: Option<driver_send::Stream>,
	recv: Option<driver_recv::Stream>,
	typ: VarRead,
	session: VarRead,
	typ_value: Option<u64>,
}

impl PendingBi {
	fn new(send: driver_send::Stream, recv: driver_recv::Stream) -> Self {
		Self {
			send: Some(send),
			recv: Some(recv),
			typ: VarRead::default(),
			session: VarRead::default(),
			typ_value: None,
		}
	}

	/// `Some(session)` for a WebTransport stream, `None` for anything else.
	fn poll_classify(&mut self, cx: &mut Context<'_>) -> Poll<Option<u64>> {
		let typ = match self.typ_value {
			Some(typ) => typ,
			None => match ready!(self.typ.poll(cx, self.recv.as_mut().expect("not yet classified"))) {
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
		match ready!(self.session.poll(cx, self.recv.as_mut().expect("not yet classified"))) {
			VarPoll::Value(session) => Poll::Ready(Some(session)),
			VarPoll::End => Poll::Ready(None),
		}
	}
}

impl Future for PendingBi {
	type Output = (Option<u64>, driver_send::Stream, driver_recv::Stream);
	fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
		let class = ready!(self.poll_classify(cx));
		Poll::Ready((
			class,
			self.send.take().expect("not yet classified"),
			self.recv.take().expect("not yet classified"),
		))
	}
}

/// Poll only newly arrived or woken headers.
fn classify_bi(session_id: u64, state: &mut State, cx: &mut Context<'_>) -> bool {
	let mut progressed = false;
	while let Poll::Ready(Some((class, send, recv))) = state.pending_bi.poll_next_unpin(cx) {
		if class == Some(session_id) {
			state.ready_bi.push_back((send, recv));
			progressed = true;
		}
	}
	progressed
}

// ── Handshake plumbing ──────────────────────────────────────────────

async fn write_all(send: &mut driver_send::Stream, buf: &[u8]) -> Result<(), Error> {
	Ok(send.write_all(buf).await?)
}

/// Read another chunk into `buf`; `false` once the stream has ended.
///
/// Fails rather than letting `buf` grow past [`MESSAGE_LIMIT`], which is what
/// bounds a peer that declares a huge frame and never finishes it.
async fn read_some(recv: &mut driver_recv::Stream, buf: &mut BytesMut) -> Result<bool, Error> {
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
async fn read_settings(recv: &mut driver_recv::Stream) -> Result<proto::Settings, Error> {
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
async fn read_connect(recv: &mut driver_recv::Stream) -> Result<proto::ConnectRequest, Error> {
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
async fn read_capsules(web: Rc<Web>, conn: Connection, mut recv: driver_recv::Stream) {
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

impl From<send::Error> for Error {
	fn from(err: send::Error) -> Self {
		match err {
			send::Error::SessionError(err) => err,
			send::Error::Stopped(code) => Self::Stop(code.into()),
			err => Self::Web(err.to_string()),
		}
	}
}

impl std::fmt::Debug for Handles {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.conn.fmt(f)
	}
}

/// Decode only this stream code registry; raw QUIC also accepts older peers' mapped codes.
fn decode_stream_code(raw: bool, code: VarInt) -> Option<u32> {
	proto::error_from_http3(code.into_inner()).or_else(|| raw.then(|| u32::try_from(code.into_inner()).ok()).flatten())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A code with a WebTransport meaning comes back out as itself; one
	/// without is HTTP/3's own failure, and must stop advertising itself as an
	/// application error through the trait accessors.
	#[test]
	fn an_unmappable_code_is_not_an_application_error() {
		use moq_net::transport::Error as _;

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

	#[test]
	fn stream_codes_preserve_their_registry() {
		let mapped = http3_code(proto::error_to_http3(5));
		for raw in [false, true] {
			assert_eq!(decode_stream_code(raw, mapped), Some(5));
			assert_eq!(decode_stream_code(raw, http3_code(1 << 40)), None);
		}
		assert_eq!(decode_stream_code(true, 5u32.into()), Some(5));
		assert_eq!(decode_stream_code(false, 5u32.into()), None);
	}

	/// The CONNECT request is logged here, so its `Debug` must keep credentials
	/// out: `web-transport-proto` redacts them from 0.6.4.
	#[test]
	fn connect_debug_redacts_credentials() {
		let url = url::Url::parse("https://user:hunter2@relay.example/anon?jwt=SECRET#SECRET").unwrap();
		let request = proto::ConnectRequest::new(url)
			.with_header(
				http::header::AUTHORIZATION,
				http::HeaderValue::from_static("Bearer SECRET"),
			)
			.with_header(http::header::COOKIE, http::HeaderValue::from_static("session=SECRET"));
		let debug = format!("{request:?}");
		assert!(debug.contains("relay.example/anon"), "{debug}");
		for secret in ["SECRET", "hunter2", "jwt="] {
			assert!(!debug.contains(secret), "{secret} leaked: {debug}");
		}
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
