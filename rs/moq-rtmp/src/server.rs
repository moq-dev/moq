//! RTMP server: accept connections, and hand each pending request to the caller
//! as a [`Request`] to authorize.
//!
//! [`Server::accept`] runs the RTMP handshake and the connect command exchange
//! for each TCP connection (many concurrently, so a slow client doesn't block
//! others), then yields a [`Request`] once the client issues its `publish` or
//! `play` command. The caller inspects the app and stream key, makes an
//! authorization decision, and either:
//!
//! - **[`Request::Publish`]**: [`Publish::accept`] (ingest into an origin at a
//!   path) or [`Publish::reject`]. This is the contribution path (OBS, ffmpeg).
//! - **[`Request::Play`]**: [`Play::accept`] (serve a broadcast from an origin
//!   down to the player) or [`Play::reject`]. This is the egress path: a player
//!   (VLC, ffplay, mpv) pulls `rtmp://host/<app>/<key>` and we stream it back.
//!
//! This mirrors `moq-native`'s `Server` / `Request`, so the gateway stays
//! unopinionated about auth: the embedder (e.g. a relay verifying the stream key
//! as a JWT) owns that policy.
//!
//! RTMPS (RTMP over TLS): [`Server::with_tls`] makes the listener terminate TLS
//! before the RTMP handshake, so `rtmps://` clients work with no other change.
//! If you'd rather own the transport (custom TLS, a non-TCP socket, a test
//! pipe), accept the connection and complete any handshake yourself, then hand
//! the established stream to [`accept_stream`]; everything here is generic over
//! the [`Stream`] trait.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::StreamExt;
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use moq_mux::container::flv::{Export as FlvExport, Import as FlvImport};
use moq_net::{BroadcastInfo, OriginConsumer, OriginProducer, OriginPublish};
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult};
use rml_rtmp::time::RtmpTimestamp;
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

use crate::Result;
use crate::flv;

/// Read buffer size for pulling RTMP chunk-stream bytes off the socket.
const READ_BUFFER: usize = 16 * 1024;

/// How long a connection has to finish the handshake and issue its `publish` or
/// `play` before it is dropped. Bounds the lifetime (and socket / `pending` slot)
/// of a client that connects but never does either, so idle or half-open
/// connections can't accumulate without limit. With TLS this also covers the TLS
/// handshake.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// TCP keepalive idle period before the kernel starts probing a silent peer, and
/// the interval between probes. Once a connection is publishing or playing it can
/// block in a `read` indefinitely, so without keepalive a half-open connection (a
/// peer that vanished without sending a FIN/RST, e.g. a yanked network cable)
/// would pin its broadcast (and its first-publisher stream-key slot) forever.
/// Keepalive lets the kernel surface the dead peer as a read error, tearing the
/// session down. The values are generous enough not to disturb a healthy but
/// momentarily quiet connection.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// A bidirectional byte stream carrying an RTMP session.
///
/// A plaintext [`tokio::net::TcpStream`] for `rtmp://`, or a TLS stream you've
/// accepted for `rtmps://`. Implemented for every
/// `AsyncRead + AsyncWrite + Unpin + Send`, so [`accept_stream`] and
/// [`Request`] work over whatever transport you bring.
pub trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// A connection accepted by [`Server`]: plaintext RTMP, or RTMPS over TLS.
///
/// This is the stream type behind a [`Server`]-produced [`Request`] (hence
/// `Request<Conn>`). Bring-your-own-transport callers using [`accept_stream`]
/// keep their own stream type instead.
pub enum Conn {
	/// A plaintext TCP connection (`rtmp://`).
	Plain(TcpStream),

	/// A TLS connection (`rtmps://`), established by [`Server::with_tls`]. Boxed
	/// because a `TlsStream` is large relative to a bare `TcpStream`.
	#[cfg(feature = "server")]
	Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_read(cx, buf),
		}
	}
}

impl AsyncWrite for Conn {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_write(cx, buf),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_flush(cx),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_flush(cx),
		}
	}

	fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
			#[cfg(feature = "server")]
			Conn::Tls(s) => Pin::new(s).poll_shutdown(cx),
		}
	}
}

/// An RTMP server that yields each connection's pending request as a [`Request`].
///
/// Build it with [`bind`](Self::bind), optionally enable RTMPS with
/// [`with_tls`](Self::with_tls), then loop on [`accept`](Self::accept). The
/// handshake and the connect exchange happen inside `accept`, so a [`Request`] is
/// only produced once a client actually wants to publish or play.
pub struct Server {
	listener: TcpListener,

	/// When set, each accepted connection is TLS-terminated (RTMPS) before the
	/// RTMP handshake.
	#[cfg(feature = "server")]
	tls: Option<tokio_rustls::TlsAcceptor>,

	/// In-flight handshakes; each resolves to a ready [`Request`], or `None` if
	/// the connection closed or errored before issuing a publish or play.
	pending: FuturesUnordered<BoxFuture<'static, Option<Request<Conn>>>>,
}

impl Server {
	/// Bind an RTMP listener on `addr` (RTMP's well-known port is 1935).
	pub async fn bind(addr: SocketAddr) -> Result<Self> {
		let listener = TcpListener::bind(addr).await?;
		Ok(Self {
			listener,
			#[cfg(feature = "server")]
			tls: None,
			pending: FuturesUnordered::new(),
		})
	}

	/// Terminate TLS on every accepted connection, turning this into an RTMPS
	/// listener (`rtmps://`). Pass a `rustls::ServerConfig` (e.g. from
	/// [`moq_native::tls::Server::server_config`] with an empty ALPN list), or
	/// `None` to leave it plaintext.
	#[cfg(feature = "server")]
	pub fn with_tls(mut self, tls: impl Into<Option<std::sync::Arc<rustls::ServerConfig>>>) -> Self {
		self.tls = tls.into().map(tokio_rustls::TlsAcceptor::from);
		self
	}

	/// The local address the listener is bound to.
	pub fn local_addr(&self) -> Result<SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	/// Wait for the next connection that wants to publish or play.
	///
	/// New connections are accepted and handshaked concurrently; this returns the
	/// next one to reach its `publish` or `play` command. Connections that close or
	/// error before either are dropped without surfacing here. Returns `None` only
	/// if the listener itself stops (it currently never does).
	pub async fn accept(&mut self) -> Option<Request<Conn>> {
		loop {
			tokio::select! {
				// A handshake finished: yield its request, or skip a dead connection.
				Some(maybe) = self.pending.next(), if !self.pending.is_empty() => {
					if let Some(request) = maybe {
						return Some(request);
					}
				}
				// A new TCP connection: start its (TLS +) handshake concurrently.
				res = self.listener.accept() => match res {
					Ok((stream, peer)) => {
						configure_socket(&stream, peer);
						#[cfg(feature = "server")]
						let tls = self.tls.clone();
						self.pending.push(Box::pin(async move {
							// The TLS handshake (if any) and the RTMP handshake share one
							// budget, so a client that stalls either is dropped.
							let outcome = tokio::time::timeout(REQUEST_TIMEOUT, async move {
								#[cfg(feature = "server")]
								let conn = match tls {
									Some(acceptor) => Conn::Tls(Box::new(
										acceptor
											.accept(stream)
											.await
											.map_err(|e| anyhow::anyhow!("rtmps tls handshake: {e}"))?,
									)),
									None => Conn::Plain(stream),
								};
								#[cfg(not(feature = "server"))]
								let conn = Conn::Plain(stream);
								accept_until_request(conn, peer).await
							})
							.await;
							match outcome {
								Ok(Ok(request)) => request,
								Ok(Err(err)) => {
									tracing::warn!(%peer, %err, "RTMP connection closed before publish/play");
									None
								}
								Err(_) => {
									tracing::warn!(%peer, "RTMP connection did not publish or play before timeout");
									None
								}
							}
						}));
					}
					Err(err) => {
						// A failed accept must not take the listener down; back off so a
						// persistent error doesn't busy-spin.
						tracing::warn!(%err, "failed to accept RTMP connection; continuing");
						tokio::time::sleep(Duration::from_millis(100)).await;
					}
				},
			}
		}
	}
}

/// Tune a freshly accepted RTMP socket: Nagle off for latency, keepalive on so a
/// dead peer is reaped rather than pinning a broadcast forever.
///
/// Both are best-effort: a failure to set either is logged and ignored rather
/// than dropping an otherwise healthy connection.
fn configure_socket(stream: &TcpStream, peer: SocketAddr) {
	// Nagle off: RTMP is latency-sensitive and we write whole packets.
	if let Err(err) = stream.set_nodelay(true) {
		tracing::debug!(%peer, %err, "failed to set TCP_NODELAY");
	}
	let keepalive = TcpKeepalive::new()
		.with_time(KEEPALIVE_IDLE)
		.with_interval(KEEPALIVE_INTERVAL);
	if let Err(err) = SockRef::from(stream).set_tcp_keepalive(&keepalive) {
		tracing::debug!(%peer, %err, "failed to set TCP keepalive");
	}
}

/// Run the RTMP handshake and connect exchange on an already-established byte
/// stream, yielding the pending publish or play as a [`Request`].
///
/// The bring-your-own-transport entry point: accept the connection (and, for
/// `rtmps://`, complete the TLS handshake) yourself, then hand the stream here.
/// `peer` is the remote address, used for logging and [`Request::peer`].
///
/// Returns `Ok(None)` if the client disconnects before issuing `publish` or
/// `play`. Unlike [`Server`], this applies no timeout: wrap the call in
/// [`tokio::time::timeout`] to bound how long a connected-but-idle client can
/// hold the task.
pub async fn accept_stream<S: Stream>(stream: S, peer: SocketAddr) -> Result<Option<Request<S>>> {
	Ok(accept_until_request(stream, peer).await?)
}

/// What an accepted RTMP connection wants: to contribute media ([`Publish`]) or
/// to view it ([`Play`]).
///
/// Yielded by [`Server::accept`] / [`accept_stream`] once the client issues its
/// `publish` or `play` command. Inspect [`app`](Self::app) /
/// [`stream_key`](Self::stream_key), then match to authorize the right
/// direction. Dropping it without accepting or rejecting closes the connection.
///
/// `S` is the underlying stream: [`Conn`] for a [`Server`]-produced request, or
/// your own transport when built via [`accept_stream`].
#[non_exhaustive]
pub enum Request<S = Conn> {
	/// A client pushing media in (OBS, ffmpeg). Ingest it with [`Publish::accept`].
	Publish(Publish<S>),
	/// A client pulling media out (VLC, ffplay, mpv). Serve it with [`Play::accept`].
	Play(Play<S>),
}

impl<S: Stream> Request<S> {
	/// The RTMP app name (the path component of `rtmp://host/<app>/<key>`).
	pub fn app(&self) -> &str {
		match self {
			Request::Publish(r) => r.app(),
			Request::Play(r) => r.app(),
		}
	}

	/// The RTMP stream key (the final component of `rtmp://host/<app>/<key>`).
	pub fn stream_key(&self) -> &str {
		match self {
			Request::Publish(r) => r.stream_key(),
			Request::Play(r) => r.stream_key(),
		}
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		match self {
			Request::Publish(r) => r.peer(),
			Request::Play(r) => r.peer(),
		}
	}
}

/// A pending RTMP publish (contribution), waiting on the caller to authorize it.
///
/// Inspect [`app`](Self::app) and [`stream_key`](Self::stream_key) (an
/// `rtmp://host/<app>/<key>` URL splits into these), then either
/// [`accept`](Self::accept) the publish into an origin at a chosen broadcast
/// path or [`reject`](Self::reject) it. Dropping it without either closes the
/// connection.
///
/// `S` is the underlying stream: [`Conn`] for a [`Server`]-produced request, or
/// your own transport when built via [`accept_stream`].
pub struct Publish<S = Conn> {
	stream: S,
	session: ServerSession,
	/// The `rml_rtmp` request id for the pending publish, replied to on accept/reject.
	request_id: u32,
	/// Session results produced alongside the publish command, processed once the
	/// publish is accepted.
	work: VecDeque<ServerSessionResult>,
	app: String,
	stream_key: String,
	peer: SocketAddr,
}

impl<S: Stream> Publish<S> {
	/// The RTMP app name (the path component of `rtmp://host/<app>/<key>`).
	pub fn app(&self) -> &str {
		&self.app
	}

	/// The RTMP stream key (the final component of `rtmp://host/<app>/<key>`).
	///
	/// Conventionally a publish secret; an embedder can treat it as a token (e.g.
	/// a moq-token JWT) to authenticate the publish.
	pub fn stream_key(&self) -> &str {
		&self.stream_key
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		self.peer
	}

	/// Accept the publish: announce a broadcast at `path` in `origin` and pump the
	/// RTMP media into it until the client disconnects.
	///
	/// `origin` is whatever the caller wants the media published into (e.g. a
	/// relay's shared origin, optionally re-rooted/scoped per the authenticated
	/// token). This future resolves when the connection ends, so callers usually
	/// run it on its own task.
	pub async fn accept(mut self, origin: &OriginProducer, path: &str) -> Result<()> {
		// Reserve the broadcast path before telling the client the publish succeeded:
		// if the origin refuses `path`, reject cleanly instead of accepting and then
		// dropping the connection a moment later.
		let mut publisher = match Publisher::new(origin, path) {
			Ok(publisher) => publisher,
			Err(err) => {
				tracing::warn!(peer = %self.peer, %path, %err, "rejecting RTMP publish: broadcast unavailable");
				let results = self
					.session
					.reject_request(self.request_id, "NetStream.Publish.Denied", "broadcast unavailable")
					.map_err(|e| anyhow::anyhow!("rtmp reject publish: {e:?}"))?;
				for result in self.work.drain(..).chain(results) {
					if let ServerSessionResult::OutboundResponse(packet) = result {
						self.stream.write_all(&packet.bytes).await?;
					}
				}
				return Ok(());
			}
		};

		let results = self
			.session
			.accept_request(self.request_id)
			.map_err(|e| anyhow::anyhow!("rtmp accept publish: {e:?}"))?;
		self.work.extend(results);
		tracing::info!(peer = %self.peer, %path, "rtmp publish accepted");

		let result = pump(
			&mut self.stream,
			&mut self.session,
			&mut self.work,
			&mut publisher,
			self.peer,
		)
		.await;

		// Flush the importer so the final groups close cleanly before unannouncing.
		if let Err(err) = publisher.finish() {
			tracing::debug!(peer = %self.peer, %err, "error finishing RTMP publish");
		}

		Ok(result?)
	}

	/// Reject the publish, sending `reason` back to the client as the
	/// `NetStream.Publish.Denied` description, then close the connection.
	pub async fn reject(mut self, reason: &str) -> Result<()> {
		let results = self
			.session
			.reject_request(self.request_id, "NetStream.Publish.Denied", reason)
			.map_err(|e| anyhow::anyhow!("rtmp reject publish: {e:?}"))?;

		// Flush any pending writes plus the rejection so it reaches the client.
		for result in self.work.drain(..).chain(results) {
			if let ServerSessionResult::OutboundResponse(packet) = result {
				self.stream.write_all(&packet.bytes).await?;
			}
		}
		tracing::debug!(peer = %self.peer, %reason, "rtmp publish rejected");
		Ok(())
	}
}

/// A pending RTMP play (egress), waiting on the caller to authorize it.
///
/// The viewing counterpart of [`Publish`]: inspect [`app`](Self::app) /
/// [`stream_key`](Self::stream_key), then [`accept`](Self::accept) to serve a
/// broadcast from an origin down to the player, or [`reject`](Self::reject) it.
/// Dropping it without either closes the connection.
///
/// `S` is the underlying stream: [`Conn`] for a [`Server`]-produced request, or
/// your own transport when built via [`accept_stream`].
pub struct Play<S = Conn> {
	stream: S,
	session: ServerSession,
	/// The `rml_rtmp` request id for the pending play, replied to on accept/reject.
	request_id: u32,
	/// The RTMP message stream id to address outbound media at (from the `play`).
	stream_id: u32,
	/// Session results produced alongside the play command, processed on accept.
	work: VecDeque<ServerSessionResult>,
	app: String,
	stream_key: String,
	peer: SocketAddr,
}

impl<S: Stream> Play<S> {
	/// The RTMP app name (the path component of `rtmp://host/<app>/<key>`).
	pub fn app(&self) -> &str {
		&self.app
	}

	/// The RTMP stream key (the final component of `rtmp://host/<app>/<key>`).
	///
	/// As with a publish, an embedder can treat this as a token to authorize the
	/// viewer.
	pub fn stream_key(&self) -> &str {
		&self.stream_key
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		self.peer
	}

	/// Accept the play: subscribe to the broadcast at `path` in `origin`, mux it
	/// to FLV, and stream the tags down to the player until either side ends.
	///
	/// Waits for the broadcast to be announced (so a player can connect slightly
	/// before the publisher), cancelling cleanly if the viewer disconnects first.
	/// This future resolves when playback ends, so callers usually run it on its
	/// own task.
	pub async fn accept(mut self, origin: &OriginConsumer, path: &str) -> Result<()> {
		// Tell the client playback is starting (Play.Reset / Play.Start + StreamBegin).
		let results = self
			.session
			.accept_request(self.request_id)
			.map_err(|e| anyhow::anyhow!("rtmp accept play: {e:?}"))?;
		self.work.extend(results);
		flush_outbound(&mut self.stream, &mut self.work).await?;

		tracing::info!(peer = %self.peer, %path, "rtmp play accepted");

		// Wait for the broadcast, but abandon the wait if the viewer hangs up. Feed
		// the client's bytes through the session (not discard them) so its
		// deserializer stays in sync for everything `play_pump` parses next.
		let broadcast = tokio::select! {
			biased;
			res = feed_input(&mut self.stream, &mut self.session, &mut self.work) => {
				res?;
				tracing::debug!(peer = %self.peer, %path, "viewer disconnected before play started");
				return Ok(());
			}
			broadcast = origin.announced_broadcast(path) => broadcast,
		};
		let Some(broadcast) = broadcast else {
			tracing::debug!(peer = %self.peer, %path, "play broadcast unavailable");
			return Ok(());
		};

		let mut export = FlvExport::new(broadcast)
			.await
			.map_err(|e| anyhow::anyhow!("init FLV export: {e}"))?;
		let result = play_pump(
			&mut self.stream,
			&mut self.session,
			&mut self.work,
			&mut export,
			self.stream_id,
			self.peer,
		)
		.await;

		tracing::debug!(peer = %self.peer, %path, "rtmp play ended");
		result
	}

	/// Reject the play, sending `reason` back to the client as the
	/// `NetStream.Play.Failed` description, then close the connection.
	pub async fn reject(mut self, reason: &str) -> Result<()> {
		let results = self
			.session
			.reject_request(self.request_id, "NetStream.Play.Failed", reason)
			.map_err(|e| anyhow::anyhow!("rtmp reject play: {e:?}"))?;

		for result in self.work.drain(..).chain(results) {
			if let ServerSessionResult::OutboundResponse(packet) = result {
				self.stream.write_all(&packet.bytes).await?;
			}
		}
		tracing::debug!(peer = %self.peer, %reason, "rtmp play rejected");
		Ok(())
	}
}

/// Run one connection's handshake and connect exchange, returning a [`Request`]
/// once the client issues `publish` or `play` (or `None` if it disconnects first).
async fn accept_until_request<S: Stream>(mut stream: S, peer: SocketAddr) -> anyhow::Result<Option<Request<S>>> {
	let remaining = run_handshake(&mut stream, peer).await?;

	let (mut session, initial) =
		ServerSession::new(ServerSessionConfig::new()).map_err(|e| anyhow::anyhow!("rtmp session init: {e:?}"))?;
	let mut work: VecDeque<ServerSessionResult> = VecDeque::from(initial);

	// Any RTMP bytes bundled with the final handshake packet.
	if !remaining.is_empty() {
		let results = session
			.handle_input(&remaining)
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		work.extend(results);
	}

	let mut buffer = [0u8; READ_BUFFER];
	loop {
		while let Some(result) = work.pop_front() {
			match result {
				ServerSessionResult::OutboundResponse(packet) => {
					stream.write_all(&packet.bytes).await?;
				}
				ServerSessionResult::RaisedEvent(event) => match event {
					// Accept every connect; authorization happens at publish/play time.
					ServerSessionEvent::ConnectionRequested { request_id, app_name } => {
						tracing::debug!(%peer, %app_name, "rtmp connect");
						let results = session
							.accept_request(request_id)
							.map_err(|e| anyhow::anyhow!("rtmp accept connect: {e:?}"))?;
						work.extend(results);
					}
					// The client wants to publish: hand control back to the caller.
					ServerSessionEvent::PublishStreamRequested {
						request_id,
						app_name,
						stream_key,
						..
					} => {
						return Ok(Some(Request::Publish(Publish {
							stream,
							session,
							request_id,
							work,
							app: app_name,
							stream_key,
							peer,
						})));
					}
					// The client wants to play: hand control back to the caller.
					ServerSessionEvent::PlayStreamRequested {
						request_id,
						app_name,
						stream_key,
						stream_id,
						..
					} => {
						return Ok(Some(Request::Play(Play {
							stream,
							session,
							request_id,
							stream_id,
							work,
							app: app_name,
							stream_key,
							peer,
						})));
					}
					other => tracing::trace!(%peer, ?other, "ignoring RTMP event before publish/play"),
				},
				ServerSessionResult::UnhandleableMessageReceived(_) => {
					tracing::trace!(%peer, "ignoring unhandleable RTMP message");
				}
			}
		}

		let n = stream.read(&mut buffer).await?;
		if n == 0 {
			return Ok(None);
		}
		let results = session
			.handle_input(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		work.extend(results);
	}
}

/// Pump RTMP media into the publisher until the client disconnects or finishes.
async fn pump<S: Stream>(
	stream: &mut S,
	session: &mut ServerSession,
	work: &mut VecDeque<ServerSessionResult>,
	publisher: &mut Publisher,
	peer: SocketAddr,
) -> anyhow::Result<()> {
	let mut buffer = [0u8; READ_BUFFER];
	loop {
		let mut finished = false;
		while let Some(result) = work.pop_front() {
			match result {
				ServerSessionResult::OutboundResponse(packet) => {
					stream.write_all(&packet.bytes).await?;
				}
				ServerSessionResult::RaisedEvent(event) => match event {
					// A frame that fails to demux is dropped, not fatal: the importer
					// consumes whole tags atomically, so one bad frame doesn't desync
					// the stream, and tearing down a live publish over it would be worse.
					ServerSessionEvent::AudioDataReceived { data, timestamp, .. } => {
						if let Err(err) = publisher.push(flv::TAG_AUDIO, timestamp.value, &data) {
							tracing::warn!(%peer, %err, "dropping RTMP audio frame that failed to demux");
						}
					}
					ServerSessionEvent::VideoDataReceived { data, timestamp, .. } => {
						if let Err(err) = publisher.push(flv::TAG_VIDEO, timestamp.value, &data) {
							tracing::warn!(%peer, %err, "dropping RTMP video frame that failed to demux");
						}
					}
					ServerSessionEvent::PublishStreamFinished { .. } => finished = true,
					// onMetaData and other script data: the FLV importer reads codec
					// config from the sequence headers, so metadata isn't forwarded.
					ServerSessionEvent::StreamMetadataChanged { .. } => {}
					other => tracing::trace!(%peer, ?other, "ignoring RTMP event"),
				},
				ServerSessionResult::UnhandleableMessageReceived(_) => {
					tracing::trace!(%peer, "ignoring unhandleable RTMP message");
				}
			}
		}
		if finished {
			break;
		}

		let n = stream.read(&mut buffer).await?;
		if n == 0 {
			break;
		}
		let results = session
			.handle_input(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		work.extend(results);
	}

	tracing::debug!(%peer, "rtmp connection closed");
	Ok(())
}

/// Stream a broadcast to an RTMP player until the broadcast ends or the viewer
/// stops.
///
/// Pulls FLV from `export`, splits it back into tags, and sends each as an RTMP
/// audio/video message; concurrently it services client input (acknowledgements,
/// pings, `deleteStream`) so a long playback stays healthy. The read and write
/// halves run independently, so media keeps flowing regardless of when the viewer
/// next sends anything.
async fn play_pump<S: Stream>(
	stream: &mut S,
	session: &mut ServerSession,
	work: &mut VecDeque<ServerSessionResult>,
	export: &mut FlvExport,
	stream_id: u32,
	peer: SocketAddr,
) -> Result<()> {
	let (mut reader, mut writer) = tokio::io::split(stream);
	let mut tags = flv::TagReader::new();
	let mut buffer = [0u8; READ_BUFFER];

	loop {
		// Flush responses queued by the last batch of client input.
		while let Some(result) = work.pop_front() {
			match result {
				ServerSessionResult::OutboundResponse(packet) => writer.write_all(&packet.bytes).await?,
				ServerSessionResult::RaisedEvent(ServerSessionEvent::PlayStreamFinished { .. }) => {
					tracing::debug!(%peer, "viewer stopped playback");
					return Ok(());
				}
				ServerSessionResult::RaisedEvent(other) => {
					tracing::trace!(%peer, ?other, "ignoring RTMP event during play")
				}
				ServerSessionResult::UnhandleableMessageReceived(_) => {}
			}
		}

		tokio::select! {
			// Media from the broadcast: split into tags and send each one down.
			chunk = export.next() => match chunk? {
				Some(bytes) => {
					tags.push(&bytes);
					while let Some(tag) = tags.next()? {
						let ts = RtmpTimestamp::new(tag.timestamp);
						let packet = match tag.tag_type {
							flv::TAG_VIDEO => session.send_video_data(stream_id, tag.body, ts, false),
							flv::TAG_AUDIO => session.send_audio_data(stream_id, tag.body, ts, false),
							_ => continue,
						}
						.map_err(|e| anyhow::anyhow!("rtmp send media: {e:?}"))?;
						writer.write_all(&packet.bytes).await?;
					}
				}
				// Broadcast ended: tell the player and finish.
				None => {
					let packet = session
						.finish_playing(stream_id)
						.map_err(|e| anyhow::anyhow!("rtmp finish play: {e:?}"))?;
					writer.write_all(&packet.bytes).await?;
					return Ok(());
				}
			},
			// Client input: feed the session so it can ack / tear down.
			res = reader.read(&mut buffer) => {
				let n = res?;
				if n == 0 {
					return Ok(());
				}
				let results = session
					.handle_input(&buffer[..n])
					.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
				work.extend(results);
			}
		}
	}
}

/// Write every queued [`OutboundResponse`](ServerSessionResult::OutboundResponse)
/// to the client, dropping the other result kinds.
async fn flush_outbound<S: Stream>(stream: &mut S, work: &mut VecDeque<ServerSessionResult>) -> anyhow::Result<()> {
	for result in work.drain(..) {
		if let ServerSessionResult::OutboundResponse(packet) = result {
			stream.write_all(&packet.bytes).await?;
		}
	}
	Ok(())
}

/// Feed client bytes through the session while we wait for the broadcast, until
/// the viewer hangs up or stops.
///
/// Returns `Ok(())` when the client closes the connection (EOF) or issues a
/// `play` teardown, so the caller can abandon the play. Crucially it does *not*
/// discard the bytes: RTMP is a single continuous chunk stream, so skipping any
/// bytes would desynchronize the session's deserializer for everything
/// [`play_pump`] parses afterwards. Pre-playback the client's control messages
/// (window ack, set buffer length) need no reply, so any responses are left
/// queued in `work` for `play_pump` to flush rather than written here. The only
/// await is the read, so dropping this future when the broadcast arrives is
/// cancellation-safe (no half-consumed read).
async fn feed_input<S: Stream>(
	stream: &mut S,
	session: &mut ServerSession,
	work: &mut VecDeque<ServerSessionResult>,
) -> anyhow::Result<()> {
	let mut buffer = [0u8; READ_BUFFER];
	loop {
		let n = stream.read(&mut buffer).await?;
		if n == 0 {
			return Ok(());
		}
		let results = session
			.handle_input(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		// The viewer tore down the play before media started: stop waiting.
		let stopped = results.iter().any(|r| {
			matches!(
				r,
				ServerSessionResult::RaisedEvent(ServerSessionEvent::PlayStreamFinished { .. })
			)
		});
		work.extend(results);
		if stopped {
			return Ok(());
		}
	}
}

/// Perform the RTMP server handshake, returning any leftover bytes that followed
/// the client's final handshake packet (the start of the chunk stream).
async fn run_handshake<S: Stream>(stream: &mut S, peer: SocketAddr) -> anyhow::Result<Vec<u8>> {
	let mut handshake = Handshake::new(PeerType::Server);
	let p0_p1 = handshake
		.generate_outbound_p0_and_p1()
		.map_err(|e| anyhow::anyhow!("rtmp handshake p0/p1: {e:?}"))?;
	stream.write_all(&p0_p1).await?;

	let mut buffer = [0u8; 4096];
	loop {
		let n = stream.read(&mut buffer).await?;
		if n == 0 {
			anyhow::bail!("peer {peer} closed during handshake");
		}

		match handshake
			.process_bytes(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handshake: {e:?}"))?
		{
			HandshakeProcessResult::InProgress { response_bytes } => {
				if !response_bytes.is_empty() {
					stream.write_all(&response_bytes).await?;
				}
			}
			HandshakeProcessResult::Completed {
				response_bytes,
				remaining_bytes,
			} => {
				if !response_bytes.is_empty() {
					stream.write_all(&response_bytes).await?;
				}
				tracing::debug!(%peer, "rtmp handshake complete");
				return Ok(remaining_bytes);
			}
		}
	}
}

/// An active publish: the moq-mux FLV importer (which owns the
/// [`BroadcastProducer`](moq_net::BroadcastProducer) it publishes into) plus the
/// origin announcement. Dropping it closes and unannounces the broadcast.
struct Publisher {
	/// Held to keep the broadcast announced for the publisher's lifetime.
	_publish: OriginPublish,
	importer: FlvImport,
}

impl Publisher {
	/// Open a broadcast at `path` and prime the importer with the FLV file
	/// header, so subsequent tags decode against an initialized demuxer.
	fn new(origin: &OriginProducer, path: &str) -> anyhow::Result<Self> {
		let mut broadcast = BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast)?;
		let mut importer = FlvImport::new(broadcast.clone(), catalog);

		let publish = origin.publish_broadcast(path, broadcast.consume())?;

		// Feed the FLV file header once up front; media tags follow per message.
		importer.decode(&flv::file_header())?;

		Ok(Self {
			_publish: publish,
			importer,
		})
	}

	/// Re-wrap one RTMP audio/video message body as an FLV tag and demux it.
	fn push(&mut self, tag_type: u8, timestamp: u32, body: &[u8]) -> anyhow::Result<()> {
		// FLV's tag DataSize is 24-bit. A larger body would truncate, declaring a
		// wrong size that desyncs the demuxer on the next tag. Drop it instead.
		anyhow::ensure!(
			body.len() <= 0xFF_FFFF,
			"RTMP message body {} exceeds FLV's 24-bit tag size limit",
			body.len()
		);
		self.importer.decode(&flv::tag(tag_type, timestamp, body))
	}

	/// Flush any buffered media and close out the broadcast's open groups.
	fn finish(&mut self) -> anyhow::Result<()> {
		self.importer.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use rml_rtmp::sessions::{
		ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
	};

	/// What the test client asks for once connected.
	#[derive(Clone, Copy)]
	enum ClientMode {
		Publish,
		Play,
	}

	/// Drive a real RTMP client over an already-connected `stream` through
	/// handshake -> connect(`live`) -> publish/play(`cam0`), pumping until aborted
	/// by the test. Generic over the transport so the same client exercises both
	/// plaintext RTMP and RTMPS.
	async fn run_client<S: Stream>(mut stream: S, mode: ClientMode) {
		// Handshake.
		let mut handshake = Handshake::new(PeerType::Client);
		stream
			.write_all(&handshake.generate_outbound_p0_and_p1().unwrap())
			.await
			.unwrap();
		let mut buffer = [0u8; 4096];
		let remaining = loop {
			let n = stream.read(&mut buffer).await.unwrap();
			match handshake.process_bytes(&buffer[..n]).unwrap() {
				HandshakeProcessResult::InProgress { response_bytes } => {
					if !response_bytes.is_empty() {
						stream.write_all(&response_bytes).await.unwrap();
					}
				}
				HandshakeProcessResult::Completed {
					response_bytes,
					remaining_bytes,
				} => {
					if !response_bytes.is_empty() {
						stream.write_all(&response_bytes).await.unwrap();
					}
					break remaining_bytes;
				}
			}
		};

		let (mut session, initial) = ClientSession::new(ClientSessionConfig::new()).unwrap();
		let mut work: VecDeque<ClientSessionResult> = VecDeque::from(initial);
		if !remaining.is_empty() {
			work.extend(session.handle_input(&remaining).unwrap());
		}
		work.push_back(session.request_connection("live".to_string()).unwrap());

		loop {
			while let Some(result) = work.pop_front() {
				match result {
					ClientSessionResult::OutboundResponse(packet) => {
						stream.write_all(&packet.bytes).await.unwrap();
					}
					// Once connected, ask to publish or play; the command is sent
					// automatically as the createStream round trip completes.
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
						let result = match mode {
							ClientMode::Publish => session
								.request_publishing("cam0".to_string(), PublishRequestType::Live)
								.unwrap(),
							ClientMode::Play => session.request_playback("cam0".to_string()).unwrap(),
						};
						work.push_back(result);
					}
					_ => {}
				}
			}
			let n = match stream.read(&mut buffer).await {
				Ok(n) => n,
				Err(_) => return,
			};
			if n == 0 {
				return;
			}
			match session.handle_input(&buffer[..n]) {
				Ok(results) => work.extend(results),
				Err(_) => return,
			}
		}
	}

	/// A received RTMP media message: `is_video` and the message body.
	type Media = (bool, bytes::Bytes);

	/// Drive an RTMP play client over `stream` through handshake -> connect(`live`)
	/// -> play(`cam0`), collecting media messages until `want` have arrived.
	async fn play_client_collect<S: Stream>(mut stream: S, want: usize) -> Vec<Media> {
		// Handshake.
		let mut handshake = Handshake::new(PeerType::Client);
		stream
			.write_all(&handshake.generate_outbound_p0_and_p1().unwrap())
			.await
			.unwrap();
		let mut buffer = [0u8; 4096];
		let remaining = loop {
			let n = stream.read(&mut buffer).await.unwrap();
			match handshake.process_bytes(&buffer[..n]).unwrap() {
				HandshakeProcessResult::InProgress { response_bytes } => {
					if !response_bytes.is_empty() {
						stream.write_all(&response_bytes).await.unwrap();
					}
				}
				HandshakeProcessResult::Completed {
					response_bytes,
					remaining_bytes,
				} => {
					if !response_bytes.is_empty() {
						stream.write_all(&response_bytes).await.unwrap();
					}
					break remaining_bytes;
				}
			}
		};

		let (mut session, initial) = ClientSession::new(ClientSessionConfig::new()).unwrap();
		let mut work: VecDeque<ClientSessionResult> = VecDeque::from(initial);
		if !remaining.is_empty() {
			work.extend(session.handle_input(&remaining).unwrap());
		}
		work.push_back(session.request_connection("live".to_string()).unwrap());

		let mut media = Vec::new();
		loop {
			while let Some(result) = work.pop_front() {
				match result {
					ClientSessionResult::OutboundResponse(packet) => {
						stream.write_all(&packet.bytes).await.unwrap();
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
						work.push_back(session.request_playback("cam0".to_string()).unwrap());
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::VideoDataReceived { data, .. }) => {
						media.push((true, data));
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::AudioDataReceived { data, .. }) => {
						media.push((false, data));
					}
					_ => {}
				}
			}
			if media.len() >= want {
				return media;
			}
			let n = stream.read(&mut buffer).await.unwrap();
			if n == 0 {
				return media;
			}
			work.extend(session.handle_input(&buffer[..n]).unwrap());
		}
	}

	/// End-to-end play: publish a real broadcast into an origin (via the FLV
	/// importer, so it carries a catalog + frames), then drive an RTMP play client
	/// and assert it receives the muxed AVC sequence header and keyframe back.
	#[tokio::test]
	async fn play_streams_broadcast_to_client() {
		// An AVC sequence-header tag body: keyframe + AVC CodecID, AVCPacketType 0,
		// composition time 0, then a minimal avcC (one SPS, one PPS).
		let avcc = {
			let sps = [0x67u8, 0x42, 0xc0, 0x1f];
			let mut out = vec![0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, sps.len() as u8];
			out.extend_from_slice(&sps);
			out.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
			out
		};
		let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
		vseq.extend_from_slice(&avcc);
		// A keyframe NALU tag body: AVCPacketType 1, then a length-prefixed IDR.
		let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00];
		vframe.extend_from_slice(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);

		// Publish the broadcast at `live/cam0` by feeding synthetic FLV to the importer.
		let origin = moq_net::Origin::random().produce();
		let mut broadcast = BroadcastInfo::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast).unwrap();
		let mut importer = FlvImport::new(broadcast.clone(), catalog);
		let _publish = origin.publish_broadcast("live/cam0", broadcast.consume()).unwrap();
		importer.decode(&flv::file_header()).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
		importer.finish().unwrap();

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();
		let consumer = origin.consume();

		// Serve the first (play) request against the populated origin.
		let server_task = tokio::spawn(async move {
			let request = server.accept().await.expect("a request");
			let Request::Play(play) = request else {
				panic!("expected a play request");
			};
			play.accept(&consumer, "live/cam0").await.unwrap();
		});

		let stream = TcpStream::connect(addr).await.unwrap();
		let media = tokio::time::timeout(Duration::from_secs(5), play_client_collect(stream, 2))
			.await
			.expect("play client timed out");

		assert!(
			media.len() >= 2,
			"expected the seq header and a keyframe, got {}",
			media.len()
		);
		// First video message is the AVC sequence header (AVCPacketType 0).
		assert!(media[0].0, "first message should be video");
		assert_eq!(media[0].1[0], 0x17);
		assert_eq!(media[0].1[1], 0x00);
		// Second is the keyframe NALU (AVCPacketType 1).
		assert!(media[1].0, "second message should be video");
		assert_eq!(media[1].1[1], 0x01);

		server_task.abort();
	}

	#[tokio::test]
	async fn accept_yields_publish_request() {
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();

		let client = tokio::spawn(async move {
			let stream = TcpStream::connect(addr).await.unwrap();
			run_client(stream, ClientMode::Publish).await;
		});

		let request = tokio::time::timeout(Duration::from_secs(5), server.accept())
			.await
			.expect("server.accept timed out")
			.expect("server yielded a request");

		assert_eq!(request.app(), "live");
		assert_eq!(request.stream_key(), "cam0");

		let Request::Publish(publish) = request else {
			panic!("expected a publish request");
		};
		publish.reject("test rejection").await.unwrap();
		client.abort();
	}

	#[tokio::test]
	async fn accept_yields_play_request() {
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();

		let client = tokio::spawn(async move {
			let stream = TcpStream::connect(addr).await.unwrap();
			run_client(stream, ClientMode::Play).await;
		});

		let request = tokio::time::timeout(Duration::from_secs(5), server.accept())
			.await
			.expect("server.accept timed out")
			.expect("server yielded a request");

		assert_eq!(request.app(), "live");
		assert_eq!(request.stream_key(), "cam0");

		let Request::Play(play) = request else {
			panic!("expected a play request");
		};
		play.reject("test rejection").await.unwrap();
		client.abort();
	}

	/// The same publish flow, but over TLS: prove [`Server::with_tls`] terminates
	/// RTMPS and yields an identical [`Request`]. Gated on `quinn` because it
	/// borrows moq-native's cert generation (`server_config`), which needs a
	/// moq-native backend feature.
	#[cfg(feature = "quinn")]
	#[tokio::test]
	async fn rtmps_accept_yields_publish_request() {
		use std::sync::Arc;

		use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
		use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
		use rustls::{DigitallySignedStruct, SignatureScheme};

		// Accept any server cert: the test uses a throwaway self-signed cert.
		#[derive(Debug)]
		struct NoVerify(Arc<rustls::crypto::CryptoProvider>);

		impl ServerCertVerifier for NoVerify {
			fn verify_server_cert(
				&self,
				_end_entity: &CertificateDer<'_>,
				_intermediates: &[CertificateDer<'_>],
				_server_name: &ServerName<'_>,
				_ocsp: &[u8],
				_now: UnixTime,
			) -> std::result::Result<ServerCertVerified, rustls::Error> {
				Ok(ServerCertVerified::assertion())
			}

			fn verify_tls12_signature(
				&self,
				message: &[u8],
				cert: &CertificateDer<'_>,
				dss: &DigitallySignedStruct,
			) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
				rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
			}

			fn verify_tls13_signature(
				&self,
				message: &[u8],
				cert: &CertificateDer<'_>,
				dss: &DigitallySignedStruct,
			) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
				rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
			}

			fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
				self.0.signature_verification_algorithms.supported_schemes()
			}
		}

		let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());

		// Server: a self-signed cert for `localhost`, fronting the RTMP listener.
		let mut tls = moq_native::tls::Server::default();
		tls.generate = vec!["localhost".to_string()];
		let server_config = tls.server_config(vec![]).expect("build RTMPS server config");

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_tls(server_config);
		let addr = server.local_addr().unwrap();

		// Client: TLS-connect (no verify), then run the ordinary RTMP client.
		let client = tokio::spawn(async move {
			let client_config = rustls::ClientConfig::builder_with_provider(provider.clone())
				.with_safe_default_protocol_versions()
				.unwrap()
				.dangerous()
				.with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
				.with_no_client_auth();
			let connector = tokio_rustls::TlsConnector::from(Arc::new(client_config));
			let tcp = TcpStream::connect(addr).await.unwrap();
			let server_name = ServerName::try_from("localhost").unwrap();
			let stream = connector.connect(server_name, tcp).await.unwrap();
			run_client(stream, ClientMode::Publish).await;
		});

		let request = tokio::time::timeout(Duration::from_secs(5), server.accept())
			.await
			.expect("server.accept timed out")
			.expect("server yielded a request");

		assert_eq!(request.app(), "live");
		assert_eq!(request.stream_key(), "cam0");

		let Request::Publish(publish) = request else {
			panic!("expected a publish request");
		};
		publish.reject("test rejection").await.unwrap();
		client.abort();
	}
}
