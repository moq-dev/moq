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
//! This mirrors `moq-tokio`'s `Server` / `Request`, so the gateway stays
//! unopinionated about auth: the embedder (e.g. a relay verifying the stream key
//! as a JWT) owns that policy.
//!
//! RTMPS (RTMP over TLS): [`Server::with_tls`] makes the listener terminate TLS
//! before the RTMP handshake for any client that opens with a ClientHello, so
//! `rtmps://` and `rtmp://` clients share one port with no other change.
//! [`Server::with_plaintext`] can refuse the plaintext ones.
//! If you'd rather own the transport (custom TLS, a non-TCP socket, a test
//! pipe), accept the connection and complete any handshake yourself, then hand
//! the established stream to [`accept_stream`]; everything here is generic over
//! the [`Stream`] trait.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use crate::rml::amf0::Amf0Value;
use crate::rml::handshake::{Handshake, HandshakeProcessResult, PeerType};
use crate::rml::sessions::{
	FourCcSupport, ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use crate::rml::time::RtmpTimestamp;
use futures::StreamExt;
use futures::future::{AbortHandle, Abortable, BoxFuture};
use futures::stream::FuturesUnordered;
use hang::catalog::{AudioCodec, AudioCodecKind, VideoCodecKind, VideoConfig};
use moq_mux::catalog::{CatalogFormat, Stream as CatalogStream};
use moq_mux::container::flv::{Export as FlvExport, Import as FlvImport};
use moq_mux::select;
use moq_net::origin;
use socket2::{SockRef, TcpKeepalive};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

use crate::Result;
use crate::flv;
use rand::RngExt;

/// Read buffer size for pulling RTMP chunk-stream bytes off the socket.
const READ_BUFFER: usize = 16 * 1024;

/// How long a connection has to finish the handshake and issue its `publish` or
/// `play` before it is dropped. Bounds the lifetime (and socket / `pending` slot)
/// of a client that connects but never does either, so idle or half-open
/// connections can't accumulate without limit. With TLS this also covers the TLS
/// handshake.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Maximum number of accepted sockets that can be handshaking or waiting for a
/// publish/play request at once; a new socket evicts the oldest waiter.
const MAX_PENDING_REQUESTS: usize = 128;

/// Budget for the TLS and RTMP handshake, before any application request.
pub(crate) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum gap between media packets from an accepted publisher or a dialed pull.
///
/// A listener's play session has no equivalent,
/// which is why [`configure_socket`]'s keepalive is not optional.
pub const PUBLISH_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a play request may wait for its broadcast and initial FLV header
/// before the server rejects it.
const PLAY_RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// TCP keepalive idle period before the kernel starts probing a silent peer, and
/// the interval between probes. Once a connection is publishing or playing it can
/// block in a `read` indefinitely, so without keepalive a half-open connection (a
/// peer that vanished without sending a FIN/RST, e.g. a yanked network cable)
/// would pin its broadcast forever.
/// Keepalive lets the kernel surface the dead peer as a read error, tearing the
/// session down. The values are generous enough not to disturb a healthy but
/// momentarily quiet connection.
const KEEPALIVE_IDLE: Duration = Duration::from_secs(30);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

// Enhanced-RTMP FourCcInfoMask bit: this gateway forwards the codec. We demux FLV
// into MoQ without decoding, so `CanForward` (not `CanDecode`) is the honest bit.
const FOURCC_CAN_FORWARD: f64 = 0x04 as f64;

// Enhanced-RTMP FourCC codecs the FLV importer accepts on ingest. Keep in sync
// with `moq_mux::container::flv::Import`; an encoder that reads our advertised
// support may gate whether it sends these.
const INGEST_VIDEO_FOURCCS: [&[u8; 4]; 4] = [b"avc1", b"hvc1", b"av01", b"vp09"];
const INGEST_AUDIO_FOURCCS: [&[u8; 4]; 5] = [b"Opus", b"mp4a", b".mp3", b"ac-3", b"ec-3"];

// Enhanced-RTMP `capsEx` (CapsExMask) bit for multitrack audio/video: several
// tracks in one RTMP stream. We ingest and egress multitrack, so we advertise it.
const CAPS_EX_MULTITRACK: u32 = 0x02;

/// The enhanced-RTMP `capsEx` capabilities this gateway supports. We handle
/// multitrack on both ingest (demux several tracks) and egress (mux several
/// renditions); reconnect and the ModEx timestamp extensions are not supported.
const SUPPORTED_CAPS_EX: u32 = CAPS_EX_MULTITRACK;

#[derive(Clone, Debug, Default)]
struct ClientCapabilities {
	multitrack: bool,
	video: FourCcSupport,
	audio: FourCcSupport,
}

impl ClientCapabilities {
	fn new(caps_ex: u32, video: FourCcSupport, audio: FourCcSupport) -> Self {
		Self {
			multitrack: caps_ex & CAPS_EX_MULTITRACK != 0,
			video,
			audio,
		}
	}

	fn supports_video(&self, fourcc: &[u8; 4]) -> bool {
		self.video.any || self.video.fourccs.contains(fourcc)
	}

	fn supports_audio(&self, fourcc: &[u8; 4]) -> bool {
		self.audio.any || self.audio.fourccs.contains(fourcc)
	}
}

/// Enhanced-RTMP capabilities to echo in the connect `_result` command object, so
/// an enhanced encoder knows it may send these FourCC codecs on ingest and that
/// multitrack is negotiated.
fn advertised_connect_properties() -> HashMap<String, Amf0Value> {
	fn info_map(fourccs: &[&[u8; 4]]) -> Amf0Value {
		Amf0Value::Object(
			fourccs
				.iter()
				.map(|fourcc| {
					(
						String::from_utf8_lossy(*fourcc).into_owned(),
						Amf0Value::Number(FOURCC_CAN_FORWARD),
					)
				})
				.collect(),
		)
	}

	HashMap::from([
		("videoFourCcInfoMap".to_string(), info_map(&INGEST_VIDEO_FOURCCS)),
		("audioFourCcInfoMap".to_string(), info_map(&INGEST_AUDIO_FOURCCS)),
		("capsEx".to_string(), Amf0Value::Number(SUPPORTED_CAPS_EX as f64)),
	])
}

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
#[non_exhaustive]
pub enum Conn {
	/// A plaintext TCP connection (`rtmp://`).
	Plain(TcpStream),

	/// A TLS connection (`rtmps://`), sniffed and terminated by [`Server::with_tls`]. Boxed
	/// because a `TlsStream` is large relative to a bare `TcpStream`.
	#[cfg(feature = "tls")]
	Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

impl AsyncRead for Conn {
	fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_read(cx, buf),
			#[cfg(feature = "tls")]
			Conn::Tls(s) => Pin::new(s).poll_read(cx, buf),
		}
	}
}

impl AsyncWrite for Conn {
	fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_write(cx, buf),
			#[cfg(feature = "tls")]
			Conn::Tls(s) => Pin::new(s).poll_write(cx, buf),
		}
	}

	fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_flush(cx),
			#[cfg(feature = "tls")]
			Conn::Tls(s) => Pin::new(s).poll_flush(cx),
		}
	}

	fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
		match self.get_mut() {
			Conn::Plain(s) => Pin::new(s).poll_shutdown(cx),
			#[cfg(feature = "tls")]
			Conn::Tls(s) => Pin::new(s).poll_shutdown(cx),
		}
	}
}

/// Backoff bounds after a failed `accept`. The listener is supervised for the process's lifetime,
/// so there is no give-up budget: the descriptor pressure or firewall rule behind a failed accept
/// clears on its own, and the next connection resets the escalation.
const ACCEPT_RETRY_MIN: Duration = Duration::from_millis(100);
const ACCEPT_RETRY_MAX: Duration = Duration::from_secs(5);

/// An RTMP server that yields each connection's pending request as a [`Request`].
///
/// Build it with [`bind`](Self::bind), optionally enable RTMPS with `with_tls`
/// (the `tls` feature), then loop on [`accept`](Self::accept). The
/// handshake and the connect exchange happen inside `accept`, so a [`Request`] is
/// only produced once a client actually wants to publish or play.
pub struct Server {
	listener: TcpListener,

	/// When set, each accepted connection that opens with a ClientHello is
	/// TLS-terminated (RTMPS) before the RTMP handshake.
	#[cfg(feature = "tls")]
	tls: Option<tokio_rustls::TlsAcceptor>,

	/// Whether a connection that is not TLS is served as plaintext RTMP.
	plaintext: bool,

	/// In-flight handshakes; each resolves to a ready [`Request`], or `None` if
	/// the connection closed or errored before issuing a publish or play.
	pending: FuturesUnordered<BoxFuture<'static, (u64, Option<Request<Conn>>)>>,
	pending_order: BTreeMap<u64, AbortHandle>,
	next_pending: u64,

	/// Delay after a failed `accept`, doubling per consecutive failure. Lives on the server rather
	/// than inside [`accept`](Self::accept) so the escalation survives across calls, and resets on
	/// the next connection that does come in.
	accept_delay: Duration,

	/// While set, `accept` stops asking the listener until this instant. In-flight handshakes keep
	/// being polled meanwhile: a connection that already got through must not wait out a backoff
	/// earned by a different one.
	accept_retry: Option<moq_clock::Instant>,
}

impl Server {
	/// Bind an RTMP listener on `addr` (RTMP's well-known port is 1935).
	pub async fn bind(addr: SocketAddr) -> Result<Self> {
		let listener = TcpListener::bind(addr).await?;

		Ok(Self {
			listener,
			#[cfg(feature = "tls")]
			tls: None,
			plaintext: true,
			pending: FuturesUnordered::new(),
			pending_order: BTreeMap::new(),
			next_pending: 0,
			accept_delay: ACCEPT_RETRY_MIN,
			accept_retry: None,
		})
	}

	/// Serve RTMPS (`rtmps://`) alongside plaintext RTMP on this one port: a
	/// connection that opens with a TLS ClientHello is TLS-terminated, any other is
	/// served as plaintext unless [`with_plaintext`](Self::with_plaintext) refuses it.
	/// Pass a `rustls::ServerConfig` (e.g. from
	/// `moq_tokio::tls::Listen::server_config` with an empty ALPN list), or
	/// `None` to serve plaintext only.
	#[cfg(feature = "tls")]
	pub fn with_tls(mut self, tls: impl Into<Option<std::sync::Arc<rustls::ServerConfig>>>) -> Self {
		self.tls = tls.into().map(tokio_rustls::TlsAcceptor::from);
		self
	}

	/// Whether to accept plaintext `rtmp://` (the default). Pass `false` to refuse it and
	/// serve `rtmps://` only, so stream keys never cross the network unencrypted: a
	/// client that does not open with a TLS ClientHello is dropped at its first byte.
	/// That needs `with_tls`, or [`accept`](Self::accept) refuses to run.
	pub fn with_plaintext(mut self, allow: bool) -> Self {
		self.plaintext = allow;
		self
	}

	/// Refuse a server that would accept no connection at all: plaintext turned off with
	/// no TLS to serve instead.
	pub(crate) fn check(&self) -> Result<()> {
		#[cfg(feature = "tls")]
		let tls = self.tls.is_some();
		#[cfg(not(feature = "tls"))]
		let tls = false;

		if !self.plaintext && !tls {
			return Err(crate::Error::Session(
				"plaintext RTMP is refused but no TLS config is set, so no client could connect".to_string(),
			));
		}
		Ok(())
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
	/// if the listener itself stops (it currently never does) or the server refuses
	/// plaintext without TLS, which is logged as an error.
	pub async fn accept(&mut self) -> Option<Request<Conn>> {
		if let Err(err) = self.check() {
			tracing::error!(%err, "RTMP server misconfigured; not accepting connections");
			return None;
		}

		loop {
			// Copied out so the timer arm below doesn't borrow `self` alongside the other two.
			let retry = self.accept_retry;

			tokio::select! {
				biased;
				// Drain cancelled handshakes before accepting another socket, bounding the futures too.
				// A handshake finished: yield its request, or skip a dead connection.
				Some((id, maybe)) = self.pending.next(), if !self.pending.is_empty() => {
					self.pending_order.remove(&id);
					if let Some(request) = maybe {
						return Some(request);
					}
				}
				// A failed accept's backoff elapsed: start asking the listener again.
				() = sleep_until(retry), if retry.is_some() => self.accept_retry = None,

				// A new TCP connection: start its (TLS +) handshake concurrently. Paused while a
				// failed accept is backing off, so a persistent error doesn't busy-spin.
				res = self.listener.accept(), if retry.is_none() => match res {
					Ok((stream, peer)) => {
						// A connection got through, so whatever the last failure was has cleared.
						self.accept_delay = ACCEPT_RETRY_MIN;
						configure_socket(&stream, peer);
						#[cfg(feature = "tls")]
						let (tls, plaintext) = (self.tls.clone(), self.plaintext);
						if self.pending_order.len() == MAX_PENDING_REQUESTS {
							let (_, oldest) = self.pending_order.pop_first().unwrap();
							oldest.abort();
						}
						let id = self.next_pending;
						self.next_pending = self.next_pending.wrapping_add(1);
						let (abort, registration) = AbortHandle::new_pair();
						self.pending_order.insert(id, abort);
						self.pending.push(Box::pin(async move {
							let request = Abortable::new(async move {
								// The TLS handshake (if any) and the RTMP handshake share one
								// budget, so a client that stalls either is dropped.
								let outcome = moq_clock::timeout(REQUEST_TIMEOUT, async move {
									#[cfg(feature = "tls")]
									let conn = match tls {
										Some(acceptor) if starts_tls(&stream).await? => Conn::Tls(Box::new(
											moq_clock::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream))
												.await
												.map_err(|_| anyhow::anyhow!("RTMPS handshake timed out"))?
												.map_err(|e| anyhow::anyhow!("rtmps tls handshake: {e}"))?,
										)),
										_ if !plaintext => {
											tracing::warn!(%peer, "refusing plaintext RTMP: this listener requires TLS (rtmps://)");
											return Ok(None);
										}
										_ => Conn::Plain(stream),
									};
									#[cfg(not(feature = "tls"))]
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
							}, registration).await.ok().flatten();
							(id, request)
						}));
					}
					Err(err) => {
						// A failed accept must not take the listener down: the usual causes
						// (descriptor exhaustion, a connection the firewall dropped mid-handshake)
						// are per-connection or clear on their own, and none of them is a reason to
						// stop serving. Escalate the wait so a persistent one stops busy-spinning
						// instead of retrying ten times a second forever. Recorded as a deadline
						// rather than slept on here, so the loop keeps serving in-flight handshakes
						// through the pause.
						tracing::warn!(%err, "failed to accept RTMP connection; continuing");
						let wait = self.accept_delay.mul_f64(0.5 + rand::rng().random::<f64>() / 2.0);
						self.accept_delay = (self.accept_delay * 2).min(ACCEPT_RETRY_MAX);
						self.accept_retry = Some(moq_clock::Instant::now() + wait);
					}
				},
			}
		}
	}
}

/// The first byte of a TLS record carrying a ClientHello. RTMP's C0 is `0x03` (or `0x06`
/// for the legacy encrypted variant), so one byte tells the two apart.
#[cfg(feature = "tls")]
const TLS_HANDSHAKE_RECORD: u8 = 0x16;

/// Whether the client opened with a TLS ClientHello, peeked without consuming it.
#[cfg(feature = "tls")]
async fn starts_tls(stream: &TcpStream) -> io::Result<bool> {
	let mut first = [0u8; 1];
	if moq_clock::timeout(HANDSHAKE_TIMEOUT, stream.peek(&mut first))
		.await
		.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "TLS detection timed out"))??
		== 0
	{
		return Err(io::ErrorKind::UnexpectedEof.into());
	}
	Ok(first[0] == TLS_HANDSHAKE_RECORD)
}

/// Sleep until `at`, or park forever when there is nothing to wait for.
///
/// The `select!` arm that uses this is guarded on `at` being set; the pending branch keeps the arm
/// well-formed rather than leaving the macro with a `None` to unwrap.
async fn sleep_until(at: Option<moq_clock::Instant>) {
	match at {
		Some(at) => moq_clock::sleep_until(at).await,
		None => std::future::pending().await,
	}
}

/// Tune an RTMP TCP socket (accepted by the server or dialed by the client):
/// Nagle off for latency, keepalive on so a dead peer is reaped rather than
/// pinning a broadcast forever.
///
/// [`Server`] applies this to every socket it accepts. Call it yourself when you own
/// the listener and hand the stream to [`accept_stream`]: the keepalive is the ONLY
/// thing bounding a play session whose viewer vanished without a FIN while its
/// broadcast was quiet. [`PUBLISH_IDLE_TIMEOUT`] covers the publish direction, but
/// nothing reads or writes on a silent play socket, so it would otherwise live until
/// the process restarted, holding its origin consumer open.
///
/// Both options are best-effort: a failure to set either is logged and ignored rather
/// than dropping an otherwise healthy connection.
pub fn configure_socket(stream: &TcpStream, peer: SocketAddr) {
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
/// `play`. The handshake must finish in five seconds; wrap the call in
/// [`moq_clock::timeout`] to also bound the wait for a publish or play request.
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

	/// The URL the client dialed, as its `connect` reported it in `tcUrl`.
	pub fn tc_url(&self) -> Option<&str> {
		match self {
			Request::Publish(r) => r.tc_url(),
			Request::Play(r) => r.tc_url(),
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
	tc_url: Option<String>,
	peer: SocketAddr,
	/// Retention declared on the media tracks this publish mints, or `None` for hang's
	/// own default. Override with [`with_max_age`](Self::with_max_age).
	max_age: Option<Duration>,
	/// Connection allocator each passthrough track claims its peak-hold bitrate on.
	/// Override with [`with_bandwidth`](Self::with_bandwidth).
	bandwidth: moq_net::bandwidth::Allocator,
	/// How long an unpublished broadcast stays up. Override with [`with_linger`](Self::with_linger).
	linger: Duration,
}

impl<S: Stream> Publish<S> {
	/// The RTMP app name (the path component of `rtmp://host/<app>/<key>`).
	pub fn app(&self) -> &str {
		&self.app
	}

	/// The RTMP stream key (the final component of `rtmp://host/<app>/<key>`).
	///
	/// Conventionally a publish secret; an embedder can treat it as a token (e.g.
	/// a moq-auth JWT) to authenticate the publish.
	pub fn stream_key(&self) -> &str {
		&self.stream_key
	}

	/// The URL the client dialed (`rtmp://host/<app>`), as its `connect` reported
	/// it in `tcUrl`, or `None` when it sent none. The only place a plaintext RTMP
	/// session carries the hostname it was addressed to; RTMPS has the SNI too.
	pub fn tc_url(&self) -> Option<&str> {
		self.tc_url.as_deref()
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		self.peer
	}

	/// Set how long relays keep a non-latest group of this publish's media tracks
	/// fetchable. `None` keeps hang's own default.
	///
	/// A retention budget, not a delivery one: it never makes a subscriber play further
	/// behind live, it caps how far back a FETCH can still reach. The default suits a
	/// segmented egress (HLS/DASH) reading the broadcast downstream, which may only
	/// advertise segments that are still fetchable. Lower it when nothing reads history
	/// and the memory matters.
	pub fn with_max_age(mut self, max_age: impl Into<Option<Duration>>) -> Self {
		self.max_age = max_age.into();
		self
	}

	/// Claim each ingested track's peak-hold catalog bitrate on `bandwidth`.
	///
	/// A passthrough import has no configured ceiling, so it reserves the measured
	/// maximum instead. A co-resident encoder then targets what is left of the
	/// uplink. Unlimited (the default) claims nothing a sender can follow.
	pub fn with_bandwidth(mut self, bandwidth: moq_net::bandwidth::Allocator) -> Self {
		self.bandwidth = bandwidth;
		self
	}

	/// Keep the broadcast up for `linger` after the client unpublishes, so a subscriber that
	/// arrives after the end still gets the tail. Defaults to [`moq_mux::Linger::DEFAULT`];
	/// zero closes it at once.
	pub fn with_linger(mut self, linger: Duration) -> Self {
		self.linger = linger;
		self
	}

	/// Accept the publish: announce a broadcast at `path` in `origin` and pump the
	/// RTMP media into it until the client disconnects.
	///
	/// `origin` is whatever the caller wants the media published into (e.g. a
	/// relay's shared origin, optionally re-rooted/scoped per the authenticated
	/// token). This future resolves when the connection ends, so callers usually
	/// run it on its own task.
	///
	/// A client that unpublishes (`deleteStream`) ends cleanly: the finished broadcast
	/// stays up for the [linger](Self::with_linger), and the future resolves after that.
	/// Dropping the future ends the linger early. A connection that drops without
	/// unpublishing closes the broadcast at once.
	pub async fn accept(mut self, origin: &origin::Producer, path: impl Into<moq_net::Path>) -> Result<()> {
		let path: moq_net::Path = path.into();
		// Reserve the broadcast path before telling the client the publish succeeded:
		// if the origin refuses `path`, reject cleanly instead of accepting and then
		// dropping the connection a moment later.
		let config = moq_mux::catalog::Config::default()
			.with_max_age(self.max_age)
			.with_bandwidth(self.bandwidth);
		let mut publisher = match Publisher::new(origin, path.as_str(), config) {
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

		tracing::info!(peer = %self.peer, %path, epoch = %publisher.epoch, "rtmp publish accepted");

		let result = pump(
			&mut self.stream,
			&mut self.session,
			&mut self.work,
			&mut publisher,
			self.peer,
		)
		.await;

		let peer = self.peer;
		match result {
			// A clean end: serve the finished broadcast for the linger, so a subscriber that
			// arrives after the end still gets the tail. The encoder is done, so hang up first.
			Ok(End::Unpublished) => {
				drop(self.stream);
				match publisher.finish(self.linger) {
					Ok(linger) => {
						tracing::info!(%peer, %path, linger = ?self.linger, "rtmp publish ended, serving the finished broadcast");
						linger.wait().await;
					}
					Err(err) => tracing::debug!(%peer, %err, "error finishing RTMP publish"),
				}
			}
			// The connection dropped without unpublishing: flush, then close at once.
			Ok(End::Disconnected) => {
				if let Err(err) = publisher.close() {
					tracing::debug!(%peer, %err, "error closing RTMP publish");
				}
			}
			// The pump failed (e.g. the client went silent): abort with the real cause so
			// subscribers see it instead of a bare drop's Error::Dropped.
			Err(err) => {
				publisher.abort(moq_net::Error::Transport(err.to_string()));
				return Err(err.into());
			}
		}
		Ok(())
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
	tc_url: Option<String>,
	peer: SocketAddr,
	/// How long the FLV muxer waits for a stalled group before skipping to a newer
	/// one; see [`with_max_delay`](Self::with_max_delay).
	max_delay: Duration,
	/// Enhanced-RTMP capabilities advertised by the player in its connect object.
	capabilities: ClientCapabilities,
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

	/// The URL the client dialed, as its `connect` reported it in `tcUrl`; see
	/// [`Publish::tc_url`].
	pub fn tc_url(&self) -> Option<&str> {
		self.tc_url.as_deref()
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		self.peer
	}

	/// Set the FLV release delay and stalled-group budget.
	/// Defaults to 500 ms; zero releases available frames immediately without deadline drops.
	pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
		self.max_delay = max_delay;
		self
	}

	/// Accept the play: subscribe to the broadcast at `path` in `origin`, mux it
	/// to FLV, and stream the tags down to the player until either side ends.
	///
	/// Waits for the broadcast to be announced (so a player can connect slightly
	/// before the publisher), cancelling cleanly if the viewer disconnects first.
	/// This future resolves when playback ends, so callers usually run it on its
	/// own task.
	pub async fn accept(mut self, origin: &origin::Consumer, path: impl Into<moq_net::Path>) -> Result<()> {
		let path: moq_net::Path = path.into();
		let Ok(mut following) = origin.follow(&path, None) else {
			tracing::debug!(peer = %self.peer, %path, "play broadcast unavailable");
			return self.reject("stream not found").await;
		};
		// Wait for the broadcast before telling the client playback started. Feed the
		// client's bytes through the session (not discard them) so its deserializer
		// stays in sync for everything `play_pump` parses next.
		let broadcast = tokio::select! {
			biased;
			res = feed_input(&mut self.stream, &mut self.session, &mut self.work) => {
				res?;
				tracing::debug!(peer = %self.peer, %path, "viewer disconnected before play started");
				return Ok(());
			}
			resolved = moq_clock::timeout(PLAY_RESOLVE_TIMEOUT, following.available()) => {
				match resolved {
					Ok(resolved) => resolved,
					Err(_) => {
						tracing::debug!(peer = %self.peer, %path, "play broadcast resolve timed out");
						return self.reject("stream not found").await;
					}
				}
			}
		};
		let Ok(broadcast) = broadcast else {
			tracing::debug!(peer = %self.peer, %path, "play broadcast unavailable");
			return self.reject("stream not found").await;
		};

		// Resolve the catalog and reject the play up front if the client can't handle its
		// codecs, before telling the viewer playback started.
		let mut check = moq_mux::catalog::Consumer::new(&broadcast, CatalogFormat::default())
			.await
			.map_err(|e| anyhow::anyhow!("init catalog check: {e}"))?;
		let catalog = tokio::select! {
			biased;
			res = feed_input(&mut self.stream, &mut self.session, &mut self.work) => {
				res?;
				tracing::debug!(peer = %self.peer, %path, "viewer disconnected before play started");
				return Ok(());
			}
			catalog = moq_clock::timeout(PLAY_RESOLVE_TIMEOUT, CatalogStream::next(&mut check)) => {
				match catalog {
					Ok(Ok(Some(catalog))) => catalog,
					Ok(Ok(None)) => {
						tracing::debug!(peer = %self.peer, %path, "play catalog ended before a snapshot");
						return self.reject("stream not available").await;
					}
					Ok(Err(e)) => return Err(anyhow::anyhow!("play catalog: {e}").into()),
					Err(_) => {
						tracing::debug!(peer = %self.peer, %path, "play catalog resolve timed out");
						return self.reject("stream not available").await;
					}
				}
			}
		};
		drop(check);
		let select = match play_selection(&catalog, &self.capabilities) {
			Ok(select) => select,
			Err(reason) => {
				tracing::debug!(peer = %self.peer, %path, %reason, "rejecting RTMP play: unsupported client capabilities");
				return self.reject(&reason).await;
			}
		};

		// The capability check consumed its snapshot, so the export follows a fresh
		// subscription narrowed to what the client can play. It reads the broadcast the
		// check resolved, so a publisher that reconnected since then doesn't split the
		// catalog from its media.
		let source = moq_mux::Source::new(origin.clone(), broadcast);
		let stream = source
			.catalog::<()>(CatalogFormat::default())
			.await
			.map_err(|e| anyhow::anyhow!("init FLV export: {e}"))?
			.select(select);
		let mut export = FlvExport::new(source, stream)
			.with_delay(self.max_delay)
			.with_multitrack(self.capabilities.multitrack);

		// Resolve the catalog and codec headers before Play.Start, too. Otherwise a
		// broadcast that never produces a playable FLV header looks successful to the
		// viewer but never emits media.
		let first_chunk = tokio::select! {
			biased;
			res = feed_input(&mut self.stream, &mut self.session, &mut self.work) => {
				res?;
				tracing::debug!(peer = %self.peer, %path, "viewer disconnected before play started");
				return Ok(());
			}
			chunk = moq_clock::timeout(PLAY_RESOLVE_TIMEOUT, export.next()) => {
				match chunk {
					Ok(Ok(Some(chunk))) => chunk,
					Ok(Ok(None)) => {
						tracing::debug!(peer = %self.peer, %path, "play broadcast ended before FLV header");
						return self.reject("stream not available").await;
					}
					Ok(Err(e)) => return Err(e.into()),
					Err(_) => {
						tracing::debug!(peer = %self.peer, %path, "play FLV header resolve timed out");
						return self.reject("stream not available").await;
					}
				}
			}
		};

		// Tell the client playback is starting (Play.Reset / Play.Start + StreamBegin).
		let results = self
			.session
			.accept_request(self.request_id)
			.map_err(|e| anyhow::anyhow!("rtmp accept play: {e:?}"))?;
		self.work.extend(results);
		flush_outbound(&mut self.stream, &mut self.work).await?;

		tracing::info!(peer = %self.peer, %path, "rtmp play accepted");

		let mut tags = flv::TagReader::new();
		send_flv_chunk(
			&mut self.stream,
			&mut self.session,
			&mut tags,
			self.stream_id,
			first_chunk,
		)
		.await?;

		let result = play_pump(
			&mut self.stream,
			&mut self.session,
			&mut self.work,
			&mut export,
			tags,
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

/// Narrow a play to what the client can decode, or refuse it.
///
/// A multitrack client receives every enabled rendition, so it must play them all. A
/// single-track client receives the best enabled rendition of each kind it can play.
fn play_selection(
	catalog: &moq_mux::catalog::hang::Catalog,
	capabilities: &ClientCapabilities,
) -> std::result::Result<select::Broadcast, String> {
	let video_playable = |config: &VideoConfig| plays_video(capabilities, config.codec.kind());
	let video_refused = if capabilities.multitrack {
		catalog
			.video
			.enabled()
			.map(|(_, config)| config)
			.find(|config| !video_playable(config))
	} else if catalog.video.renditions.values().any(video_playable) {
		None
	} else {
		catalog.video.ranked().next().map(|(_, config)| config)
	};
	if let Some(config) = video_refused {
		return Err(match video_fourcc(config.codec.kind(), capabilities.multitrack) {
			Some(fourcc) => format!(
				"client did not advertise required RTMP FourCC {}",
				fourcc_label(&fourcc)
			),
			None => format!("RTMP can't carry video codec {}", config.codec),
		});
	}

	let audio_playable = |config: &hang::catalog::AudioConfig| plays_audio(capabilities, &config.codec);
	let audio_refused = if capabilities.multitrack {
		catalog
			.audio
			.enabled()
			.map(|(_, config)| config)
			.find(|config| !audio_playable(config))
	} else if catalog.audio.renditions.values().any(audio_playable) {
		None
	} else {
		catalog.audio.ranked().next().map(|(_, config)| config)
	};
	if let Some(config) = audio_refused {
		return Err(match audio_fourcc(&config.codec, capabilities.multitrack) {
			Some(fourcc) => format!(
				"client did not advertise required RTMP FourCC {}",
				fourcc_label(&fourcc)
			),
			None => format!("RTMP can't carry audio codec {}", config.codec),
		});
	}

	let mut video = select::Video::default();
	let mut any_video = false;
	for kind in [
		VideoCodecKind::H264,
		VideoCodecKind::H265,
		VideoCodecKind::AV1,
		VideoCodecKind::VP9,
	] {
		if plays_video(capabilities, kind) {
			video = video.codec(kind);
			any_video = true;
		}
	}
	// An empty codec list would select every codec, so a client that plays none gets no video.
	let mut select = select::Broadcast::default();
	if any_video {
		select = select.video(video);
	}

	let mut audio = select::Audio::default();
	let mut any_audio = false;
	for kind in playable_audio_kinds(capabilities) {
		audio = audio.codec(kind);
		any_audio = true;
	}
	// Same as video: an empty list would keep codecs the client never advertised.
	if any_audio {
		select = select.audio(audio);
	}
	Ok(select)
}

/// Whether a client with `capabilities` can play `codec` over FLV.
fn plays_audio(capabilities: &ClientCapabilities, codec: &AudioCodec) -> bool {
	match audio_fourcc(codec, capabilities.multitrack) {
		Some(fourcc) => capabilities.supports_audio(&fourcc),
		// Every client plays AAC and MP3 by their legacy CodecIDs; nothing else goes without a FourCC.
		None => matches!(codec, AudioCodec::AAC(_) | AudioCodec::Mp3),
	}
}

/// Codec families this client can play.
///
/// AAC and MP3 need no FourCC on a single-track client.
fn playable_audio_kinds(capabilities: &ClientCapabilities) -> Vec<AudioCodecKind> {
	let samples = [
		AudioCodec::AAC(hang::catalog::AAC { profile: 2 }),
		AudioCodec::Mp3,
		AudioCodec::Opus,
		AudioCodec::Ac3,
		AudioCodec::Ec3,
	];
	let mut kinds = Vec::new();
	for codec in samples {
		if plays_audio(capabilities, &codec) {
			let kind = codec.kind();
			if !kinds.contains(&kind) {
				kinds.push(kind);
			}
		}
	}
	kinds
}

/// Whether a client with `capabilities` can play `kind` over FLV.
fn plays_video(capabilities: &ClientCapabilities, kind: VideoCodecKind) -> bool {
	match video_fourcc(kind, capabilities.multitrack) {
		Some(fourcc) => capabilities.supports_video(&fourcc),
		// Every client plays H.264 by its legacy CodecID; nothing else goes without a FourCC.
		None => kind == VideoCodecKind::H264,
	}
}

/// The enhanced-RTMP FourCC a client must advertise to play `kind`, if any.
fn video_fourcc(kind: VideoCodecKind, multitrack: bool) -> Option<[u8; 4]> {
	match kind {
		VideoCodecKind::H264 if multitrack => Some(*b"avc1"),
		VideoCodecKind::H265 => Some(*b"hvc1"),
		VideoCodecKind::AV1 => Some(*b"av01"),
		VideoCodecKind::VP9 => Some(*b"vp09"),
		_ => None,
	}
}

fn audio_fourcc(codec: &AudioCodec, multitrack: bool) -> Option<[u8; 4]> {
	match codec {
		AudioCodec::AAC(_) if multitrack => Some(*b"mp4a"),
		AudioCodec::Mp3 if multitrack => Some(*b".mp3"),
		AudioCodec::Opus => Some(*b"Opus"),
		AudioCodec::Ac3 => Some(*b"ac-3"),
		AudioCodec::Ec3 => Some(*b"ec-3"),
		AudioCodec::AAC(_) | AudioCodec::Mp3 | AudioCodec::Flac | AudioCodec::Mp2 | AudioCodec::Unknown(_) => None,
		_ => None,
	}
}

fn fourcc_label(fourcc: &[u8; 4]) -> String {
	String::from_utf8_lossy(fourcc).into_owned()
}

/// Run one connection's handshake and connect exchange, returning a [`Request`]
/// once the client issues `publish` or `play` (or `None` if it disconnects first).
async fn accept_until_request<S: Stream>(mut stream: S, peer: SocketAddr) -> anyhow::Result<Option<Request<S>>> {
	let remaining = moq_clock::timeout(HANDSHAKE_TIMEOUT, run_handshake(&mut stream, peer))
		.await
		.map_err(|_| anyhow::anyhow!("RTMP handshake timed out"))??;

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

	let mut client_capabilities = ClientCapabilities::default();
	let mut tc_url = None;

	let mut buffer = [0u8; READ_BUFFER];
	loop {
		while let Some(result) = work.pop_front() {
			match result {
				ServerSessionResult::OutboundResponse(packet) => {
					stream.write_all(&packet.bytes).await?;
				}
				ServerSessionResult::RaisedEvent(event) => match event {
					// Accept every connect; authorization happens at publish/play time.
					ServerSessionEvent::ConnectionRequested {
						request_id,
						app_name,
						tc_url: dialed,
						caps_ex,
						video_fourccs,
						audio_fourccs,
					} => {
						client_capabilities = ClientCapabilities::new(caps_ex, video_fourccs, audio_fourccs);
						tc_url = dialed;
						tracing::debug!(
							%peer,
							%app_name,
							tc_url = tc_url.as_deref().unwrap_or(""),
							caps_ex,
							client_multitrack = client_capabilities.multitrack,
							client_video_fourccs = client_capabilities.video.fourccs.len(),
							client_audio_fourccs = client_capabilities.audio.fourccs.len(),
							"rtmp connect"
						);
						// Advertise the enhanced-RTMP codecs we ingest, and our capsEx, in
						// the connect _result.
						session.set_connect_response_properties(advertised_connect_properties());
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
							tc_url,
							peer,
							max_age: None,
							bandwidth: moq_net::bandwidth::Allocator::unlimited(),
							linger: moq_mux::Linger::DEFAULT,
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
							tc_url,
							peer,
							max_delay: crate::DEFAULT_MAX_DELAY,
							capabilities: client_capabilities.clone(),
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

/// How a publish's connection ended without an error.
#[derive(Debug, PartialEq, Eq)]
enum End {
	/// The client unpublished (`closeStream` or `deleteStream`) after its final media.
	Unpublished,
	/// The connection closed without unpublishing first.
	Disconnected,
}

/// Pump RTMP media into the publisher until the client disconnects or finishes.
async fn pump<S: Stream>(
	stream: &mut S,
	session: &mut ServerSession,
	work: &mut VecDeque<ServerSessionResult>,
	publisher: &mut Publisher,
	peer: SocketAddr,
) -> anyhow::Result<End> {
	let mut buffer = [0u8; READ_BUFFER];
	let mut media_deadline = moq_clock::Instant::now() + PUBLISH_IDLE_TIMEOUT;
	loop {
		let mut finished = false;
		while let Some(result) = work.pop_front() {
			match result {
				ServerSessionResult::OutboundResponse(packet) => {
					moq_clock::timeout_at(media_deadline, stream.write_all(&packet.bytes))
						.await
						.map_err(|_| anyhow::anyhow!("peer blocked RTMP control output before the media deadline"))??;
				}
				ServerSessionResult::RaisedEvent(event) => match event {
					// A timestamp restart ends this publisher; isolated malformed tags are skipped.
					ServerSessionEvent::AudioDataReceived { data, timestamp, .. } => {
						media_deadline = moq_clock::Instant::now() + PUBLISH_IDLE_TIMEOUT;
						publisher.push(flv::TAG_AUDIO, timestamp.value, &data)?;
					}
					ServerSessionEvent::VideoDataReceived { data, timestamp, .. } => {
						media_deadline = moq_clock::Instant::now() + PUBLISH_IDLE_TIMEOUT;
						publisher.push(flv::TAG_VIDEO, timestamp.value, &data)?;
					}
					ServerSessionEvent::PublishStreamFinished { .. } => finished = true,
					// onMetaData and other script data: the FLV importer reads codec
					// config from the sequence headers, so script data isn't forwarded.
					ServerSessionEvent::StreamMetadataChanged { .. }
					| ServerSessionEvent::UnhandleableAmf0Data { .. } => publisher.metadata.record(),
					other => tracing::trace!(%peer, ?other, "ignoring RTMP event"),
				},
				ServerSessionResult::UnhandleableMessageReceived(_) => {
					tracing::trace!(%peer, "ignoring unhandleable RTMP message");
				}
			}
		}
		if finished {
			tracing::debug!(%peer, "rtmp publish unpublished");
			return Ok(End::Unpublished);
		}

		let n = tokio::select! {
			res = stream.read(&mut buffer) => res?,
			_ = moq_clock::sleep_until(media_deadline) => {
				anyhow::bail!("peer {peer} sent no RTMP media for {:?}", PUBLISH_IDLE_TIMEOUT);
			}
		};
		if n == 0 {
			tracing::debug!(%peer, "rtmp connection closed without unpublishing");
			return Ok(End::Disconnected);
		}
		let results = session
			.handle_input(&buffer[..n])
			.map_err(|e| anyhow::anyhow!("rtmp handle_input: {e:?}"))?;
		work.extend(results);
	}
}

/// Stream a broadcast to an RTMP player until the broadcast ends or the viewer
/// stops.
///
/// Pulls FLV from `export`, splits it back into tags, and sends each as an RTMP
/// audio/video message; concurrently it services client input (acknowledgements,
/// pings, `deleteStream`) so a long playback stays healthy. The read and write
/// halves run independently, so media keeps flowing regardless of when the viewer
/// next sends anything.
async fn play_pump<S: Stream, C: CatalogStream>(
	stream: &mut S,
	session: &mut ServerSession,
	work: &mut VecDeque<ServerSessionResult>,
	export: &mut FlvExport<C>,
	mut tags: flv::TagReader,
	stream_id: u32,
	peer: SocketAddr,
) -> Result<()> {
	let (mut reader, mut writer) = tokio::io::split(stream);
	let mut buffer = [0u8; READ_BUFFER];

	if flush_play_work(work, &mut writer, peer).await? {
		return Ok(());
	}

	loop {
		if flush_play_work(work, &mut writer, peer).await? {
			return Ok(());
		}
		tokio::select! {
			// Media from the broadcast: split into tags and send each one down.
			chunk = export.next() => match chunk? {
				Some(bytes) => send_flv_chunk(&mut writer, session, &mut tags, stream_id, bytes).await?,
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

/// Flush responses queued by RTMP client input during playback.
async fn flush_play_work<W: AsyncWrite + Unpin>(
	work: &mut VecDeque<ServerSessionResult>,
	writer: &mut W,
	peer: SocketAddr,
) -> Result<bool> {
	while let Some(result) = work.pop_front() {
		match result {
			ServerSessionResult::OutboundResponse(packet) => writer.write_all(&packet.bytes).await?,
			ServerSessionResult::RaisedEvent(ServerSessionEvent::PlayStreamFinished { .. }) => {
				tracing::debug!(%peer, "viewer stopped playback");
				return Ok(true);
			}
			ServerSessionResult::RaisedEvent(other) => {
				tracing::trace!(%peer, ?other, "ignoring RTMP event during play")
			}
			ServerSessionResult::UnhandleableMessageReceived(_) => {}
		}
	}
	Ok(false)
}

/// Convert one FLV chunk into RTMP media messages.
async fn send_flv_chunk<W: AsyncWrite + Unpin>(
	writer: &mut W,
	session: &mut ServerSession,
	tags: &mut flv::TagReader,
	stream_id: u32,
	bytes: bytes::Bytes,
) -> Result<()> {
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
	Ok(())
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

/// An active publish: the moq-mux FLV importer, which owns the origin-created
/// [`BroadcastProducer`](moq_net::origin::Publication) it publishes into.
/// [`Self::close`] or dropping it closes the broadcast and unannounces the path;
/// [`Self::finish`] hands back the finished broadcast to linger on.
struct Publisher {
	importer: FlvImport,
	// Finished at a clean end, so the finished broadcast keeps serving the catalog.
	catalog: moq_mux::catalog::Producer,
	metadata: crate::metadata::Dropped,
	// Controls publication so an end can retract before the importer's fill clone drops.
	broadcast: moq_net::origin::Publication,
	// This connection's publisher instance, so an encoder reconnecting under the
	// same key replaces this broadcast at once instead of resuming into it.
	epoch: moq_net::Epoch,
}

impl Publisher {
	/// Open a broadcast at `path` under a fresh epoch and prime the importer with
	/// the FLV file header, so subsequent tags decode against an initialized demuxer.
	fn new(origin: &origin::Producer, path: &str, config: moq_mux::catalog::Config) -> anyhow::Result<Self> {
		let epoch = moq_net::Epoch::mint();
		let broadcast = origin.publish(path, moq_net::origin::Route::default().with_epoch(epoch.clone()))?;
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), config)?;
		let handle = broadcast.clone();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());

		// Feed the FLV file header once up front; media tags follow per message.
		importer.decode(&flv::file_header())?;

		Ok(Self {
			importer,
			catalog,
			metadata: crate::metadata::Dropped::new(path),
			broadcast: handle,
			epoch,
		})
	}

	/// Re-wrap one RTMP audio/video message body as an FLV tag and demux it.
	fn push(&mut self, tag_type: u8, timestamp: u32, body: &[u8]) -> anyhow::Result<()> {
		flv::import(&mut self.importer, tag_type, timestamp, body)
	}

	/// Flush any buffered media and finish every track, the catalog included, keeping the
	/// finished broadcast up for `linger`.
	fn finish(mut self, linger: Duration) -> anyhow::Result<moq_mux::Linger<Self>> {
		self.importer.finish()?;
		self.catalog.finish()?;
		Ok(moq_mux::Linger::new([self.broadcast.broadcast().clone()], self, linger))
	}

	/// Flush any buffered media, finish the catalog, close out the broadcast's open groups, and end
	/// the broadcast so the origin unannounces it immediately.
	fn close(mut self) -> anyhow::Result<()> {
		self.importer.finish()?;
		self.catalog.finish()?;
		self.broadcast.close();
		Ok(())
	}

	/// Abort the published tracks with `err` so subscribers see the real cause
	/// (the client disconnected, a protocol error) rather than a generic
	/// `Error::Dropped` from the importer being dropped.
	///
	/// Consumes the publisher and closes the broadcast.
	fn abort(self, err: moq_net::Error) {
		self.catalog.abort(err.clone());
		self.importer.abort(err);
		self.broadcast.close();
	}
}

#[cfg(test)]
#[expect(
	clippy::disallowed_methods,
	clippy::disallowed_types,
	reason = "tests read the host clock directly"
)]
mod tests {
	use super::*;

	#[tokio::test(start_paused = true)]
	async fn a_silent_peer_cannot_hold_a_handshake() {
		let (_client, server) = tokio::io::duplex(4096);
		let start = moq_clock::Instant::now();
		let result = accept_stream(server, "127.0.0.1:1935".parse().unwrap()).await;
		assert!(result.is_err());
		assert_eq!(start.elapsed(), HANDSHAKE_TIMEOUT);
	}

	#[tokio::test(start_paused = true)]
	async fn blocked_control_output_cannot_bypass_the_publish_idle_budget() {
		let (mut stream, _peer) = tokio::io::duplex(1);
		let (mut session, _) = ServerSession::new(ServerSessionConfig::new()).unwrap();
		let (packet, _) = session.send_ping_request().unwrap();
		let mut work = VecDeque::from([ServerSessionResult::OutboundResponse(packet)]);
		let origin = moq_tokio::origin::spawn();
		let mut publisher = Publisher::new(&origin, "idle", Default::default()).unwrap();
		let start = moq_clock::Instant::now();
		assert!(
			pump(
				&mut stream,
				&mut session,
				&mut work,
				&mut publisher,
				"127.0.0.1:1935".parse().unwrap()
			)
			.await
			.is_err()
		);
		assert_eq!(start.elapsed(), PUBLISH_IDLE_TIMEOUT);
		publisher.abort(moq_net::Error::Cancel);
	}

	#[tokio::test]
	async fn a_full_pending_set_evicts_the_oldest_instead_of_stopping_accepts() {
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		for id in 0..MAX_PENDING_REQUESTS as u64 {
			let (abort, registration) = AbortHandle::new_pair();
			server.pending_order.insert(id, abort);
			server.pending.push(Box::pin(async move {
				let request = Abortable::new(std::future::pending::<Option<Request<Conn>>>(), registration)
					.await
					.ok()
					.flatten();
				(id, request)
			}));
		}
		server.next_pending = MAX_PENDING_REQUESTS as u64;
		let addr = server.local_addr().unwrap();
		let client = tokio::spawn(async move {
			run_client(TcpStream::connect(addr).await.unwrap(), ClientMode::Publish).await;
		});
		let request = server.accept().await.unwrap();
		assert!(matches!(request, Request::Publish(_)));
		assert_eq!(server.pending_order.len(), MAX_PENDING_REQUESTS - 1);
		assert_eq!(*server.pending_order.first_key_value().unwrap().0, 1);
		assert_eq!(server.pending.len(), MAX_PENDING_REQUESTS - 1);
		drop(request);
		client.await.unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn a_timestamp_rewind_ends_the_publish() {
		let (mut stream, _peer) = tokio::io::duplex(1024);
		let (mut session, _) = ServerSession::new(ServerSessionConfig::new()).unwrap();
		let origin = moq_tokio::origin::spawn();
		let mut publisher = Publisher::new(&origin, "rewind", Default::default()).unwrap();
		let mut work = VecDeque::new();
		for (timestamp, data) in [
			(0, &[0xaf, 0, 0x12, 0x10][..]),
			(0, &[0xaf, 1, 0][..]),
			(1000, &[0xaf, 1, 0][..]),
			(0, &[0xaf, 1, 0][..]),
		] {
			work.push_back(ServerSessionResult::RaisedEvent(
				ServerSessionEvent::AudioDataReceived {
					app_name: "live".into(),
					stream_key: "rewind".into(),
					data: bytes::Bytes::copy_from_slice(data),
					timestamp: RtmpTimestamp::new(timestamp),
				},
			));
		}
		work.push_back(ServerSessionResult::RaisedEvent(
			ServerSessionEvent::PublishStreamFinished {
				app_name: "live".into(),
				stream_key: "rewind".into(),
			},
		));
		let result = pump(
			&mut stream,
			&mut session,
			&mut work,
			&mut publisher,
			"127.0.0.1:1935".parse().unwrap(),
		)
		.await;
		publisher.abort(moq_net::Error::Cancel);
		assert!(result.is_err(), "a restarted timestamp domain kept publishing");
	}

	#[tokio::test(start_paused = true)]
	#[tracing_test::traced_test]
	async fn publish_metadata_drops_warn_once() {
		let (mut stream, _peer) = tokio::io::duplex(1024);
		let (mut session, _) = ServerSession::new(ServerSessionConfig::new()).unwrap();
		let origin = moq_tokio::origin::spawn();
		let mut publisher = Publisher::new(&origin, "metadata", Default::default()).unwrap();
		let mut work = VecDeque::from([ServerSessionResult::RaisedEvent(
			ServerSessionEvent::StreamMetadataChanged {
				app_name: "live".into(),
				stream_key: "metadata".into(),
				metadata: crate::rml::sessions::StreamMetadata::new(),
			},
		)]);
		// A custom script message goes through the session's own parser.
		work.extend(session.handle_input(&crate::metadata::cue_point_packet()).unwrap());
		work.push_back(ServerSessionResult::RaisedEvent(
			ServerSessionEvent::PublishStreamFinished {
				app_name: "live".into(),
				stream_key: "metadata".into(),
			},
		));
		pump(
			&mut stream,
			&mut session,
			&mut work,
			&mut publisher,
			"127.0.0.1:1935".parse().unwrap(),
		)
		.await
		.unwrap();
		assert_eq!(publisher.metadata.count, 2);
		drop(publisher.finish(Duration::ZERO).unwrap());
		logs_assert(|lines| {
			let warnings = lines
				.iter()
				.filter(|line| line.contains("discarding RTMP stream metadata"))
				.count();
			let summary = lines.iter().any(|line| {
				line.contains("RTMP metadata discard summary")
					&& line.contains("path=metadata")
					&& line.contains("count=2")
			});
			if warnings == 1 && summary {
				Ok(())
			} else {
				Err(format!("warnings={warnings}, summary={summary}"))
			}
		});
	}

	use crate::rml::sessions::{
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
		let remaining = client_handshake(&mut stream).await;
		let mut buffer = [0u8; 4096];

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

	/// How [`publish_one_keyframe`] leaves once its media is sent.
	enum Leave {
		/// `deleteStream`, then wait for the server to hang up.
		Unpublish,
		/// Close the connection without unpublishing.
		Hangup,
	}

	/// Publish one AVC keyframe as `live/cam0` over `stream`, then leave.
	async fn publish_one_keyframe<S: Stream>(mut stream: S, leave: Leave) {
		let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
		vseq.extend_from_slice(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f]);
		vseq.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
		let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00];
		vframe.extend_from_slice(&[0, 0, 0, 5, 0x65, 0x88, 0x84, 0x21, 0x00]);

		let remaining = client_handshake(&mut stream).await;
		let (mut session, initial) = ClientSession::new(ClientSessionConfig::new()).unwrap();
		let mut work: VecDeque<ClientSessionResult> = VecDeque::from(initial);
		work.extend(session.handle_input(&remaining).unwrap());
		work.push_back(session.request_connection("live".to_string()).unwrap());
		let mut buffer = [0u8; 4096];
		loop {
			while let Some(result) = work.pop_front() {
				match result {
					ClientSessionResult::OutboundResponse(packet) => stream.write_all(&packet.bytes).await.unwrap(),
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => work.push_back(
						session
							.request_publishing("cam0".to_string(), PublishRequestType::Live)
							.unwrap(),
					),
					ClientSessionResult::RaisedEvent(ClientSessionEvent::PublishRequestAccepted) => {
						for body in [&vseq, &vframe] {
							let data = bytes::Bytes::copy_from_slice(body);
							let result = session.publish_video_data(data, RtmpTimestamp::new(0), false).unwrap();
							work.push_back(result);
						}
						match leave {
							Leave::Unpublish => work.extend(session.stop_publishing().unwrap()),
							Leave::Hangup => {
								for result in work.drain(..) {
									if let ClientSessionResult::OutboundResponse(packet) = result {
										stream.write_all(&packet.bytes).await.unwrap();
									}
								}
								return;
							}
						}
					}
					_ => {}
				}
			}
			match stream.read(&mut buffer).await {
				Ok(0) | Err(_) => return,
				Ok(n) => work.extend(session.handle_input(&buffer[..n]).unwrap()),
			}
		}
	}

	/// Accept the publish arriving on `stream` into `origin` at `live/cam0`, lingering for `linger`.
	async fn accept_publish(stream: tokio::io::DuplexStream, origin: origin::Producer, linger: Duration) {
		let peer = "127.0.0.1:1935".parse().unwrap();
		let Some(Request::Publish(publish)) = accept_stream(stream, peer).await.unwrap() else {
			panic!("expected a publish request");
		};
		publish
			.with_linger(linger)
			.accept(&origin, moq_net::Path::from_static("live/cam0"))
			.await
			.unwrap();
	}

	/// An unpublish is a clean end: the finished broadcast stays up for the linger, then
	/// closes. A hangup without one closes at once.
	#[tokio::test(start_paused = true)]
	async fn an_unpublish_lingers_and_a_hangup_does_not() {
		for (leave, linger) in [
			(Leave::Unpublish, Duration::from_secs(10)),
			(Leave::Hangup, Duration::ZERO),
		] {
			let origin = moq_tokio::origin::spawn();
			let consumer = origin.consume();
			let (client, server) = tokio::io::duplex(64 * 1024);
			let start = tokio::time::Instant::now();
			let accept = tokio::spawn(accept_publish(server, origin.clone(), Duration::from_secs(10)));
			// A hangup ends the instance at once, before a follower would hand it out, so this
			// requests it on its announcement instead.
			let mut announced = consumer.announced();
			let broadcast = async {
				announced.next().await.unwrap();
				consumer.request("live/cam0", None).await.unwrap()
			};
			let (broadcast, ()) = tokio::join!(broadcast, publish_one_keyframe(client, leave));

			accept.await.unwrap();
			assert_eq!(start.elapsed(), linger);
			assert!(broadcast.is_closed());
		}
	}

	/// A hangup ends the catalog too, so a reader sees the publisher go rather than a
	/// producer dropped unfinished.
	#[tokio::test(start_paused = true)]
	async fn a_hangup_ends_the_catalog() {
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let (mut client, server) = tokio::io::duplex(64 * 1024);
		let accept = tokio::spawn(accept_publish(server, origin.clone(), Duration::ZERO));
		let catalog = async {
			let broadcast = consumer.follow("live/cam0", None).unwrap().available().await.unwrap();
			let track = broadcast.track(hang::Catalog::DEFAULT_NAME).unwrap();
			track.subscribe(None).await.unwrap()
		};
		// Holding the stream past the publish keeps the hangup until the catalog is subscribed.
		let (mut catalog, ()) = tokio::join!(catalog, publish_one_keyframe(&mut client, Leave::Hangup));
		drop(client);
		accept.await.unwrap();

		let end = loop {
			match catalog.recv_group().await {
				Ok(Some(_)) => continue,
				end => break end.map(|_| ()),
			}
		};
		assert!(end.is_ok(), "the catalog ends cleanly: {end:?}");
	}

	/// A late subscriber still reads the finished media during the linger, and an encoder
	/// reconnecting under the same key takes the path without ending it.
	#[tokio::test(start_paused = true)]
	async fn a_reconnect_leaves_the_linger_running() {
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let (client, server) = tokio::io::duplex(64 * 1024);
		let accept = tokio::spawn(accept_publish(server, origin.clone(), Duration::from_secs(10)));
		publish_one_keyframe(client, Leave::Unpublish).await;

		tokio::time::sleep(Duration::from_secs(5)).await;
		let finished = consumer.request("live/cam0", None).await.unwrap();
		let mut video = finished.track("0.flv-v").unwrap().subscribe(None).await.unwrap();
		assert!(
			video.recv_group().await.unwrap().is_some(),
			"the late subscriber reads the tail"
		);
		assert!(!accept.is_finished());

		let (client, server) = tokio::io::duplex(64 * 1024);
		let _reconnect = tokio::spawn(accept_publish(server, origin.clone(), Duration::from_secs(10)));
		let _encoder = tokio::spawn(publish_one_keyframe(client, Leave::Hangup));
		tokio::time::sleep(Duration::from_secs(1)).await;
		assert!(!accept.is_finished(), "the newer publisher leaves the linger running");
		assert!(!finished.is_closed());

		let start = tokio::time::Instant::now();
		accept.await.unwrap();
		assert_eq!(start.elapsed(), Duration::from_secs(4));
		assert!(finished.is_closed());
	}

	/// The client half of the RTMP handshake, returning the bytes read past it.
	async fn client_handshake<S: Stream>(stream: &mut S) -> Vec<u8> {
		let mut handshake = Handshake::new(PeerType::Client);
		stream
			.write_all(&handshake.generate_outbound_p0_and_p1().unwrap())
			.await
			.unwrap();
		let mut buffer = [0u8; 4096];
		loop {
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
					return remaining_bytes;
				}
			}
		}
	}

	/// A received RTMP media message: `is_video` and the message body.
	type Media = (bool, bytes::Bytes);

	fn set_connect_capabilities(session: &mut ClientSession, caps_ex: u32, fourccs: &[&[u8; 4]]) {
		let mut properties = HashMap::new();
		if caps_ex != 0 {
			properties.insert("capsEx".to_string(), Amf0Value::Number(caps_ex as f64));
		}
		if !fourccs.is_empty() {
			properties.insert(
				"fourCcList".to_string(),
				Amf0Value::StrictArray(
					fourccs
						.iter()
						.map(|fourcc| Amf0Value::Utf8String(String::from_utf8_lossy(*fourcc).into_owned()))
						.collect(),
				),
			);
		}
		if !properties.is_empty() {
			session.set_connect_request_properties(properties);
		}
	}

	/// Drive an RTMP play client over `stream`, collecting media messages until `want` have
	/// arrived or the server hangs up.
	async fn play_client_collect<S: Stream>(stream: S, want: usize, caps_ex: u32, fourccs: &[&[u8; 4]]) -> Vec<Media> {
		let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
		let client = play_client(stream, caps_ex, fourccs, tx);
		tokio::pin!(client);
		let mut media = Vec::new();
		while media.len() < want {
			tokio::select! {
				Some(next) = rx.recv() => media.push(next),
				() = &mut client => break,
			}
		}
		while media.len() < want
			&& let Ok(next) = rx.try_recv()
		{
			media.push(next);
		}
		media
	}

	/// Drive an RTMP play client over `stream` through handshake -> connect(`live`)
	/// -> play(`cam0`), sending each media message to `media` until the server hangs up.
	async fn play_client<S: Stream>(
		mut stream: S,
		caps_ex: u32,
		fourccs: &[&[u8; 4]],
		media: tokio::sync::mpsc::UnboundedSender<Media>,
	) {
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
		set_connect_capabilities(&mut session, caps_ex, fourccs);
		work.push_back(session.request_connection("live".to_string()).unwrap());

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
						let _ = media.send((true, data));
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::AudioDataReceived { data, .. }) => {
						let _ = media.send((false, data));
					}
					_ => {}
				}
			}
			let n = match stream.read(&mut buffer).await {
				Ok(0) | Err(_) => return,
				Ok(n) => n,
			};
			work.extend(session.handle_input(&buffer[..n]).unwrap());
		}
	}

	async fn play_client_first_status<S: Stream>(mut stream: S, caps_ex: u32, fourccs: &[&[u8; 4]]) -> Option<String> {
		fn raw_status(deserializer: &mut crate::rml::chunk_io::ChunkDeserializer, bytes: &[u8]) -> Option<String> {
			let mut input = bytes;
			while let Some(payload) = deserializer.get_next_message(input).unwrap() {
				input = &[];
				match payload.to_rtmp_message() {
					Ok(crate::rml::messages::RtmpMessage::SetChunkSize { size }) => {
						deserializer.set_max_chunk_size(size as usize).unwrap();
					}
					Ok(crate::rml::messages::RtmpMessage::Amf0Command {
						command_name,
						additional_arguments,
						..
					}) if command_name == "onStatus" || command_name == "_error" => {
						let Some(Amf0Value::Object(properties)) = additional_arguments.first() else {
							continue;
						};
						let Some(Amf0Value::Utf8String(code)) = properties.get("code") else {
							continue;
						};
						return Some(code.clone());
					}
					_ => {}
				}
			}
			None
		}

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
		let mut deserializer = crate::rml::chunk_io::ChunkDeserializer::new();
		if let Some(code) = raw_status(&mut deserializer, &remaining) {
			return Some(code);
		}
		if !remaining.is_empty() {
			work.extend(session.handle_input(&remaining).unwrap());
		}
		set_connect_capabilities(&mut session, caps_ex, fourccs);
		work.push_back(session.request_connection("live".to_string()).unwrap());

		loop {
			while let Some(result) = work.pop_front() {
				match result {
					ClientSessionResult::OutboundResponse(packet) => {
						stream.write_all(&packet.bytes).await.unwrap();
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
						work.push_back(session.request_playback("cam0".to_string()).unwrap());
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::PlaybackRequestAccepted) => {
						return Some("NetStream.Play.Start".to_string());
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::UnhandleableOnStatusCode { code }) => {
						return Some(code);
					}
					ClientSessionResult::RaisedEvent(ClientSessionEvent::VideoDataReceived { .. })
					| ClientSessionResult::RaisedEvent(ClientSessionEvent::AudioDataReceived { .. }) => {
						return Some("media".to_string());
					}
					_ => {}
				}
			}
			let n = stream.read(&mut buffer).await.unwrap();
			if n == 0 {
				return None;
			}
			if let Some(code) = raw_status(&mut deserializer, &buffer[..n]) {
				return Some(code);
			}
			work.extend(session.handle_input(&buffer[..n]).unwrap());
		}
	}

	/// The retention a publish declares has to reach the media tracks the FLV importer
	/// mints, not stop at the catalog producer it was set on.
	#[tokio::test]
	async fn publisher_declares_the_configured_retention() {
		let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
		vseq.extend_from_slice(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f]);
		vseq.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);

		let origin = moq_tokio::origin::spawn();
		let mut publisher = Publisher::new(
			&origin,
			"live/cam0",
			moq_mux::catalog::Config::default().with_max_age(Duration::from_secs(3)),
		)
		.unwrap();
		publisher.push(flv::TAG_VIDEO, 0, &vseq).unwrap();

		let consumer = origin.consume();
		let broadcast = consumer.follow("live/cam0", None).unwrap().available().await.unwrap();
		let info = broadcast.track("0.flv-v").unwrap().info().await.unwrap();
		assert_eq!(info.max_age, Some(Duration::from_secs(3)));
	}

	/// End-to-end play: publish a real broadcast into an origin (via the FLV importer, so it
	/// carries a catalog + frames), then drive an RTMP play client and assert it receives the
	/// muxed AVC sequence header and keyframe back.
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
		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("live/cam0").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
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
			play.accept(&consumer, moq_net::Path::from_static("live/cam0"))
				.await
				.unwrap();
		});

		let stream = TcpStream::connect(addr).await.unwrap();
		let media = tokio::time::timeout(Duration::from_secs(5), play_client_collect(stream, 2, 0, &[]))
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

	/// Publish an AVC broadcast at `live/cam0` under `epoch`, starting with a keyframe carrying
	/// `idr`, and return its importer for more tags.
	fn publish_avc(
		origin: &origin::Producer,
		epoch: moq_net::Epoch,
		idr: &[u8],
	) -> (FlvImport, moq_mux::catalog::Producer) {
		let mut vseq = vec![0x17, 0x00, 0x00, 0x00, 0x00];
		vseq.extend_from_slice(&[0x01, 0x42, 0xc0, 0x1f, 0xff, 0xe1, 0x00, 0x04, 0x67, 0x42, 0xc0, 0x1f]);
		vseq.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
		let mut vframe = vec![0x17, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, idr.len() as u8];
		vframe.extend_from_slice(idr);

		let broadcast = origin.create_broadcast("live/cam0").unwrap();
		broadcast.announce(origin::Route::default().with_epoch(epoch)).unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
		importer.decode(&flv::file_header()).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vseq)).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vframe)).unwrap();
		(importer, catalog)
	}

	/// A play stays on the instance it resolved: a publisher taking over the path never reaches
	/// it, so the session never splices in another timeline or codec state, and the play fails
	/// once its instance's route dies.
	#[tokio::test]
	async fn a_play_stays_on_its_instance_until_the_route_dies() {
		const OLD_DELTA: &[u8] = &[0x41, 0x9a, 0x02, 0x03];
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let (mut old, old_catalog) = publish_avc(&origin, moq_net::Epoch::mint(), &[0x65, 0x88, 0x84, 0x21, 0x00]);

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();
		let play_origin = consumer.clone();
		let played = tokio::spawn(async move {
			let Some(Request::Play(play)) = server.accept().await else {
				panic!("expected a play request");
			};
			play.accept(&play_origin, moq_net::Path::from_static("live/cam0")).await
		});
		let (tx, mut media) = tokio::sync::mpsc::unbounded_channel();
		let client = tokio::spawn(play_client(TcpStream::connect(addr).await.unwrap(), 0, &[], tx));
		let mut next = async || {
			tokio::time::timeout(Duration::from_secs(5), media.recv())
				.await
				.expect("no media")
				.expect("the play ended")
		};
		// The sequence header, then the keyframe.
		assert_eq!(next().await.1[..2], [0x17, 0x00]);
		assert_eq!(next().await.1[..2], [0x17, 0x01]);

		let epoch = moq_net::Epoch::mint();
		let mut instances = moq_mux::follow::Instances::new(&consumer, "live/cam0", Some(&epoch)).unwrap();
		let (_new, _new_catalog) = publish_avc(&origin, epoch, &[0x65, 0x99, 0x99, 0x99, 0x99]);
		tokio::time::timeout(Duration::from_secs(5), instances.next())
			.await
			.expect("the new publisher takes over")
			.unwrap();

		let mut delta = vec![0x27, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, OLD_DELTA.len() as u8];
		delta.extend_from_slice(OLD_DELTA);
		old.decode(&flv::tag(flv::TAG_VIDEO, 40, &delta)).unwrap();
		let (video, body) = next().await;
		assert!(video && body.starts_with(&[0x27, 0x01]), "{body:?}");
		assert!(body.ends_with(OLD_DELTA), "the play left its instance: {body:?}");

		old.abort(moq_net::Error::Transport("connection lost".into()));
		drop(old_catalog);
		let played = tokio::time::timeout(Duration::from_secs(5), played)
			.await
			.expect("the play outlived its route")
			.unwrap();
		assert!(played.is_err(), "the play ended cleanly");
		client.abort();
	}

	#[tokio::test]
	async fn play_enhanced_codec_rejects_legacy_client() {
		const VP9_KEYFRAME_320X240: &[u8] = &[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00];

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("live/cam0").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
		importer.decode(&flv::file_header()).unwrap();

		// Enhanced video CodedFrames keyframe: ex-header, keyframe, packet type 1.
		let mut vp9 = vec![0x80 | (1 << 4) | 1];
		vp9.extend_from_slice(b"vp09");
		vp9.extend_from_slice(VP9_KEYFRAME_320X240);
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &vp9)).unwrap();
		importer.finish().unwrap();

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();
		let consumer = origin.consume();

		let server_task = tokio::spawn(async move {
			let request = server.accept().await.expect("a request");
			let Request::Play(play) = request else {
				panic!("expected a play request");
			};
			play.accept(&consumer, moq_net::Path::from_static("live/cam0"))
				.await
				.unwrap();
		});

		let stream = TcpStream::connect(addr).await.unwrap();
		let status = tokio::time::timeout(Duration::from_secs(5), play_client_first_status(stream, 0, &[]))
			.await
			.expect("play client timed out");

		assert_eq!(status.as_deref(), Some("NetStream.Play.Failed"));
		server_task.await.unwrap();
	}

	/// A single-track client gets the best rendition it can decode, whatever the
	/// names; a multitrack client must decode every rendition.
	#[test]
	fn play_selection_picks_the_best_playable_rendition() {
		fn rendition(codec: impl Into<hang::catalog::VideoCodec>, height: u32) -> VideoConfig {
			let mut config = VideoConfig::new(codec);
			config.coded_width = Some(height * 16 / 9);
			config.coded_height = Some(height);
			config
		}
		let h264 = hang::catalog::H264 {
			profile: 0x42,
			constraints: 0,
			level: 0x1e,
			inline: false,
		};

		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		// Name order would pick the lowest rendition.
		catalog
			.video
			.renditions
			.insert("a".to_string(), rendition(h264.clone(), 360));
		catalog.video.renditions.insert(
			"b".to_string(),
			rendition(
				hang::catalog::H265 {
					in_band: false,
					profile_space: 0,
					profile_idc: 1,
					profile_compatibility_flags: [0x60, 0, 0, 0],
					tier_flag: false,
					level_idc: 120,
					constraint_flags: [0x90, 0, 0, 0, 0, 0],
				},
				1080,
			),
		);
		catalog.video.renditions.insert("c".to_string(), rendition(h264, 720));

		fn best(catalog: &moq_mux::catalog::hang::Catalog, capabilities: &ClientCapabilities) -> Option<String> {
			let select = play_selection(catalog, capabilities).unwrap();
			let mut catalog = catalog.clone();
			select.retain(&mut catalog);
			catalog.video.ranked().next().map(|(name, _)| name.clone())
		}

		let legacy = ClientCapabilities::default();
		assert_eq!(best(&catalog, &legacy).as_deref(), Some("c"));

		let hevc = FourCcSupport {
			any: false,
			fourccs: vec![*b"hvc1"],
		};
		let enhanced = ClientCapabilities::new(0, hevc.clone(), FourCcSupport::default());
		assert_eq!(best(&catalog, &enhanced).as_deref(), Some("b"));

		// Multitrack carries every rendition, so one the client can't decode refuses the play.
		let multitrack = ClientCapabilities::new(CAPS_EX_MULTITRACK, hevc.clone(), FourCcSupport::default());
		assert!(play_selection(&catalog, &multitrack).is_err());

		// A disabled rendition is never selected, so a single-track client gets the best enabled
		// one, and a multitrack client that can't decode it still plays the rest.
		catalog.video.renditions.get_mut("c").unwrap().enabled = false;
		assert_eq!(best(&catalog, &legacy).as_deref(), Some("a"));
		let avc = FourCcSupport {
			any: false,
			fourccs: vec![*b"avc1"],
		};
		let h264_only = ClientCapabilities::new(CAPS_EX_MULTITRACK, avc, FourCcSupport::default());
		assert!(
			play_selection(&catalog, &h264_only).is_err(),
			"b is enabled and is H.265"
		);
		catalog.video.renditions.get_mut("b").unwrap().enabled = false;
		assert!(play_selection(&catalog, &h264_only).is_ok());
		assert_eq!(best(&catalog, &h264_only).as_deref(), Some("a"));
	}

	/// A single-track client gets the best audio rendition it can play, not the first by name.
	/// A multitrack client must still play every rendition.
	#[test]
	fn play_selection_picks_the_best_playable_audio() {
		fn audio(codec: AudioCodec, bitrate: u64) -> hang::catalog::AudioConfig {
			let mut config = hang::catalog::AudioConfig::new(codec, 48_000, 2);
			config.bitrate = Some(bitrate);
			config
		}

		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		// Name order would check Opus first and refuse a legacy client that can play the AAC.
		catalog
			.audio
			.renditions
			.insert("a".to_string(), audio(AudioCodec::Opus, 128_000));
		catalog.audio.renditions.insert(
			"b".to_string(),
			audio(AudioCodec::AAC(hang::catalog::AAC { profile: 2 }), 64_000),
		);

		let legacy = ClientCapabilities::default();
		let select = play_selection(&catalog, &legacy).expect("aac is playable");
		let mut kept = catalog.clone();
		select.retain(&mut kept);
		assert_eq!(
			kept.audio.renditions.keys().map(String::as_str).collect::<Vec<_>>(),
			["b"]
		);

		// Nothing the client can play: cite the best rendition, not the first name.
		let mut unsupported = moq_mux::catalog::hang::Catalog::default();
		unsupported
			.audio
			.renditions
			.insert("a".to_string(), audio(AudioCodec::Opus, 32_000));
		unsupported
			.audio
			.renditions
			.insert("b".to_string(), audio(AudioCodec::Ac3, 128_000));
		let err = play_selection(&unsupported, &legacy).expect_err("no playable audio");
		assert!(err.contains("ac-3"), "{err}");

		// An advertised enhanced codec is kept, but not a sibling codec the client never
		// advertised, even at a higher bitrate: an AC-3 decoder can't play E-AC-3.
		let mut ac3 = moq_mux::catalog::hang::Catalog::default();
		ac3.audio
			.renditions
			.insert("a".to_string(), audio(AudioCodec::Ac3, 128_000));
		ac3.audio
			.renditions
			.insert("b".to_string(), audio(AudioCodec::Ec3, 256_000));
		let support = FourCcSupport {
			any: false,
			fourccs: vec![*b"ac-3"],
		};
		let select = play_selection(&ac3, &ClientCapabilities::new(0, FourCcSupport::default(), support)).unwrap();
		let mut kept = ac3.clone();
		select.retain(&mut kept);
		assert_eq!(
			kept.audio.renditions.keys().map(String::as_str).collect::<Vec<_>>(),
			["a"]
		);

		// Multitrack still requires every rendition.
		let multitrack = ClientCapabilities::new(
			CAPS_EX_MULTITRACK,
			FourCcSupport::default(),
			FourCcSupport {
				any: false,
				fourccs: vec![*b"Opus"],
			},
		);
		assert!(play_selection(&catalog, &multitrack).is_err());
	}

	#[test]
	fn play_capability_check_uses_per_kind_decode_support() {
		let mut catalog = moq_mux::catalog::hang::Catalog::default();
		catalog.video.renditions.insert(
			"video".to_string(),
			hang::catalog::VideoConfig::new(hang::catalog::VP9 {
				level: 10,
				bit_depth: 8,
				..Default::default()
			}),
		);

		let video_support = FourCcSupport {
			any: false,
			fourccs: vec![*b"vp09"],
		};
		assert!(
			play_selection(
				&catalog,
				&ClientCapabilities::new(0, video_support, FourCcSupport::default())
			)
			.is_ok()
		);

		let audio_support = FourCcSupport {
			any: false,
			fourccs: vec![*b"vp09"],
		};
		assert!(
			play_selection(
				&catalog,
				&ClientCapabilities::new(0, FourCcSupport::default(), audio_support)
			)
			.is_err()
		);

		let wildcard = FourCcSupport {
			any: true,
			fourccs: Vec::new(),
		};
		assert!(
			play_selection(
				&catalog,
				&ClientCapabilities::new(0, wildcard, FourCcSupport::default())
			)
			.is_ok()
		);
	}

	/// End-to-end multitrack egress: publish a broadcast carrying two H.264
	/// renditions, then a play client that advertised the multitrack `capsEx`
	/// receives enhanced-RTMP multitrack video tags for both track ids. Proves the
	/// negotiation reaches the FLV muxer.
	#[tokio::test]
	async fn play_multitrack_to_capable_client() {
		// Enhanced-RTMP multitrack framing constants, matching moq-mux's FLV muxer.
		const EX: u8 = 0x80;
		const MULTITRACK: u8 = 5;
		const SEQUENCE_START: u8 = 0;
		const CODED_FRAMES: u8 = 1;
		const MANY_TRACKS: u8 = 1;

		let avcc = |level: u8| {
			let sps = [0x67u8, 0x42, 0xc0, level];
			let mut out = vec![0x01, 0x42, 0xc0, level, 0xff, 0xe1, 0x00, sps.len() as u8];
			out.extend_from_slice(&sps);
			out.extend_from_slice(&[0x01, 0x00, 0x04, 0x68, 0xce, 0x3c, 0x80]);
			out
		};

		// Build one ManyTracks multitrack video tag body over two avc1 tracks.
		let multitrack_body = |packet_type: u8, tracks: &[(u8, Vec<u8>)]| {
			let mut body = vec![EX | (1 << 4) | MULTITRACK, (MANY_TRACKS << 4) | packet_type];
			body.extend_from_slice(b"avc1"); // shared codec
			for (track_id, payload) in tracks {
				body.push(*track_id);
				body.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]); // UI24 size
				body.extend_from_slice(payload);
			}
			body
		};

		let seq = multitrack_body(SEQUENCE_START, &[(0, avcc(0x1f)), (1, avcc(0x1e))]);
		// CodedFrames: avc1 prefixes a 3-byte composition time (zero) before the NALU.
		let nalu = |b: u8| vec![0, 0, 0, 0, 0, 5, 0x65, b, 0x84, 0x21, 0x00];
		let frames = multitrack_body(CODED_FRAMES, &[(0, nalu(0x88)), (1, nalu(0x99))]);

		let origin = moq_tokio::origin::spawn();
		let broadcast = origin.create_broadcast("live/cam0").unwrap();
		broadcast.announce(Default::default()).unwrap();
		let catalog =
			moq_mux::catalog::Producer::new(&mut broadcast.broadcast().clone(), moq_mux::catalog::Config::default())
				.unwrap();
		let mut importer = FlvImport::new(broadcast.broadcast().clone(), catalog.reserve());
		importer.decode(&flv::file_header()).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &seq)).unwrap();
		importer.decode(&flv::tag(flv::TAG_VIDEO, 0, &frames)).unwrap();
		importer.finish().unwrap();

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();
		let consumer = origin.consume();

		let server_task = tokio::spawn(async move {
			let request = server.accept().await.expect("a request");
			let Request::Play(play) = request else {
				panic!("expected a play request");
			};
			play.accept(&consumer, moq_net::Path::from_static("live/cam0"))
				.await
				.unwrap();
		});

		// The player advertises multitrack; collect the four video messages (two
		// sequence headers + two keyframes across the two tracks).
		let stream = TcpStream::connect(addr).await.unwrap();
		let media = tokio::time::timeout(
			Duration::from_secs(5),
			play_client_collect(stream, 4, CAPS_EX_MULTITRACK, &[b"avc1"]),
		)
		.await
		.expect("play client timed out");

		let video: Vec<&bytes::Bytes> = media.iter().filter(|(is_video, _)| *is_video).map(|(_, d)| d).collect();
		assert!(
			video.len() >= 2,
			"expected multitrack video messages, got {}",
			video.len()
		);
		// Every video message uses the multitrack framing (packet type 5) over avc1.
		for data in &video {
			assert_eq!(data[0] & 0x0f, MULTITRACK, "video message should be multitrack-framed");
			assert_eq!(&data[2..6], b"avc1");
		}
		// Both track ids show up (the byte after the FourCC in each OneTrack tag).
		let mut track_ids: Vec<u8> = video.iter().map(|data| data[6]).collect();
		track_ids.sort_unstable();
		track_ids.dedup();
		assert_eq!(
			track_ids,
			vec![0, 1],
			"both renditions should egress as multitrack tracks"
		);

		server_task.abort();
	}

	#[tokio::test]
	async fn play_missing_broadcast_rejects_without_start() {
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();

		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();

		let stream = TcpStream::connect(addr).await.unwrap();
		let client = tokio::spawn(async move {
			run_client(stream, ClientMode::Play).await;
		});
		let request = server.accept().await.expect("a request");
		let Request::Play(play) = request else {
			panic!("expected a play request");
		};

		tokio::time::pause();
		let server_task = tokio::spawn(async move {
			play.accept(&consumer, moq_net::Path::from_static("live/missing"))
				.await
				.unwrap();
		});

		tokio::task::yield_now().await;
		tokio::time::advance(PLAY_RESOLVE_TIMEOUT + Duration::from_millis(1)).await;
		for _ in 0..10 {
			if server_task.is_finished() {
				break;
			}
			tokio::task::yield_now().await;
		}

		if !server_task.is_finished() {
			client.abort();
			server_task.abort();
			panic!("play accept did not finish after resolve timeout");
		}
		server_task.await.unwrap();
		client.abort();
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

	/// An encoder reconnecting under the same key while its stale connection is still
	/// open restarts the broadcast at once: each connection is its own epoch, so a
	/// fresh request reaches the reconnect, while the stale viewer stays on the old one
	/// until it leaves.
	#[tokio::test]
	async fn a_reconnect_replaces_the_stale_publish() {
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let server_task = tokio::spawn(async move {
			while let Some(request) = server.accept().await {
				let Request::Publish(publish) = request else {
					panic!("expected a publish request");
				};
				let origin = origin.clone();
				tokio::spawn(async move { publish.accept(&origin, moq_net::Path::from_static("live/cam0")).await });
			}
		});

		let stale_client = tokio::spawn(async move {
			run_client(TcpStream::connect(addr).await.unwrap(), ClientMode::Publish).await;
		});
		let stale = consumer.follow("live/cam0", None).unwrap().available().await.unwrap();
		let mut catalog = stale
			.track(hang::Catalog::DEFAULT_NAME)
			.unwrap()
			.subscribe(None)
			.await
			.unwrap();

		let mut announced = consumer.announced();
		let fresh_client = tokio::spawn(async move {
			run_client(TcpStream::connect(addr).await.unwrap(), ClientMode::Publish).await;
		});
		tokio::time::timeout(Duration::from_secs(5), async {
			while !matches!(announced.next().await, Some(moq_net::announce::Event::Restart(_))) {}
		})
		.await
		.expect("the reconnect restarts the broadcast");

		let stale_viewer = tokio::time::timeout(Duration::from_millis(200), async {
			loop {
				match catalog.recv_group().await {
					Ok(Some(_)) => continue,
					other => return other.map(|_| ()),
				}
			}
		})
		.await;
		assert!(stale_viewer.is_err(), "the stale viewer ended: {stale_viewer:?}");
		assert!(!stale_client.is_finished(), "the stale connection is still open");

		let fresh = consumer.request("live/cam0", None).await.unwrap();
		assert!(!fresh.is_clone(&stale), "viewers reach the reconnected publish");

		stale_client.abort();
		fresh_client.abort();
		server_task.abort();
	}

	/// The connect `_result` should advertise the enhanced-RTMP codecs we ingest
	/// plus our `capsEx` (multitrack), so an enhanced encoder knows what it may
	/// send. Drives a raw client through the handshake, sends `connect` (itself
	/// advertising `capsEx`), and deserializes the server's `_result` to read the
	/// command object (rml's ClientSession discards those properties).
	#[tokio::test]
	async fn connect_result_advertises_capabilities() {
		use crate::rml::chunk_io::{ChunkDeserializer, ChunkSerializer};
		use crate::rml::messages::RtmpMessage;

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = server.local_addr().unwrap();

		// The server processes the connect while parked in accept() awaiting a
		// publish/play that never comes; abort it once we've read the _result.
		let server_task = tokio::spawn(async move {
			let _ = server.accept().await;
		});

		let mut stream = TcpStream::connect(addr).await.unwrap();

		// Client handshake.
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

		// Send a `connect` advertising our own capsEx (multitrack).
		let connect = RtmpMessage::Amf0Command {
			command_name: "connect".to_string(),
			transaction_id: 1.0,
			command_object: Amf0Value::Object(HashMap::from([
				("app".to_string(), Amf0Value::Utf8String("live".to_string())),
				("capsEx".to_string(), Amf0Value::Number(CAPS_EX_MULTITRACK as f64)),
			])),
			additional_arguments: Vec::new(),
		};
		let payload = connect.into_message_payload(RtmpTimestamp::new(0), 0).unwrap();
		let packet = ChunkSerializer::new().serialize(&payload, false, false).unwrap();
		stream.write_all(&packet.bytes).await.unwrap();

		// Read inbound and pull out the `_result` command object. Track SetChunkSize
		// so the multi-chunk _result reassembles correctly.
		let mut deserializer = ChunkDeserializer::new();
		let mut pending = remaining;
		let command_object = 'outer: loop {
			let mut input: &[u8] = &pending;
			while let Some(payload) = deserializer.get_next_message(input).unwrap() {
				input = &[];
				match payload.to_rtmp_message() {
					Ok(RtmpMessage::SetChunkSize { size }) => {
						deserializer.set_max_chunk_size(size as usize).unwrap();
					}
					Ok(RtmpMessage::Amf0Command {
						command_name,
						command_object,
						..
					}) if command_name == "_result" => break 'outer command_object,
					_ => {}
				}
			}
			let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
				.await
				.expect("timed out awaiting connect _result")
				.unwrap();
			assert!(n > 0, "server closed before sending connect _result");
			pending = buffer[..n].to_vec();
		};

		let Amf0Value::Object(properties) = command_object else {
			panic!("connect _result command object was not an AMF0 object");
		};
		let Some(Amf0Value::Object(video)) = properties.get("videoFourCcInfoMap") else {
			panic!("connect _result did not advertise videoFourCcInfoMap");
		};
		assert!(video.contains_key("hvc1"), "expected hvc1 in videoFourCcInfoMap");
		assert!(video.contains_key("av01"), "expected av01 in videoFourCcInfoMap");
		let Some(Amf0Value::Object(audio)) = properties.get("audioFourCcInfoMap") else {
			panic!("connect _result did not advertise audioFourCcInfoMap");
		};
		assert!(audio.contains_key("Opus"), "expected Opus in audioFourCcInfoMap");

		// The server echoes its supported capsEx, including the multitrack bit.
		let Some(Amf0Value::Number(caps_ex)) = properties.get("capsEx") else {
			panic!("connect _result did not advertise capsEx");
		};
		assert_eq!(
			*caps_ex as u32 & CAPS_EX_MULTITRACK,
			CAPS_EX_MULTITRACK,
			"connect _result should advertise the multitrack capsEx bit"
		);

		server_task.abort();
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

	/// A throwaway self-signed server config for `localhost` plus a client connector that
	/// trusts anything. The cert is generated by the `moq-tokio` dev-dependency.
	#[cfg(feature = "tls")]
	fn tls_fixture() -> (std::sync::Arc<rustls::ServerConfig>, tokio_rustls::TlsConnector) {
		use std::sync::Arc;

		use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
		use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
		use rustls::{DigitallySignedStruct, SignatureScheme};

		// Accept any server cert: the tests use a throwaway self-signed cert.
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

		let mut tls = moq_tokio::tls::Listen::default();
		tls.generate = vec!["localhost".to_string()];
		let server_config = tls.server_config(vec![]).expect("build RTMPS server config");

		let client_config = rustls::ClientConfig::builder_with_provider(provider.clone())
			.with_safe_default_protocol_versions()
			.unwrap()
			.dangerous()
			.with_custom_certificate_verifier(Arc::new(NoVerify(provider)))
			.with_no_client_auth();

		(server_config, tokio_rustls::TlsConnector::from(Arc::new(client_config)))
	}

	/// Publish over TLS: prove the server terminates RTMPS and yields an identical
	/// [`Request`].
	#[cfg(feature = "tls")]
	async fn rtmps_publish(server: &mut Server, connector: tokio_rustls::TlsConnector) {
		use rustls::pki_types::ServerName;

		let addr = server.local_addr().unwrap();
		let client = tokio::spawn(async move {
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

	/// By default a server with TLS terminates RTMPS and still serves a plaintext client on the
	/// same port. Gated on `tls` (RTMPS support).
	#[cfg(feature = "tls")]
	#[tokio::test]
	async fn rtmps_and_rtmp_share_one_port() {
		let (server_config, connector) = tls_fixture();

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_tls(server_config);
		let addr = server.local_addr().unwrap();

		rtmps_publish(&mut server, connector).await;

		let plain = tokio::spawn(async move {
			let stream = TcpStream::connect(addr).await.unwrap();
			run_client(stream, ClientMode::Play).await;
		});
		let request = tokio::time::timeout(Duration::from_secs(5), server.accept())
			.await
			.expect("plaintext accept timed out")
			.expect("server yielded a plaintext request");
		let Request::Play(play) = request else {
			panic!("expected a play request");
		};
		play.reject("test rejection").await.unwrap();
		plain.abort();
	}

	/// With plaintext turned off, a server refuses a plaintext client at its first byte,
	/// without waiting for the request timeout, and keeps serving RTMPS afterwards.
	#[cfg(feature = "tls")]
	#[tokio::test]
	async fn rtmps_required_refuses_plaintext() {
		let (server_config, connector) = tls_fixture();

		let mut server = Server::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_tls(server_config)
			.with_plaintext(false);
		let addr = server.local_addr().unwrap();

		// The connection is handled inside `accept`, so drive it while the plaintext client
		// opens with an RTMP C0.
		let plain = tokio::spawn(async move {
			let mut stream = TcpStream::connect(addr).await.unwrap();
			stream.write_all(&[0x03]).await.unwrap();
			// A refusal closes the socket: EOF, or a reset when unread bytes were pending.
			// A served client would instead be sent the handshake S0/S1.
			let mut buf = [0u8; 1];
			matches!(stream.read(&mut buf).await, Ok(0) | Err(_))
		});

		// The refusal comes well inside REQUEST_TIMEOUT, so a client that stayed open
		// (a bug) fails the outer timeout rather than passing quietly.
		let refused = tokio::select! {
			refused = tokio::time::timeout(Duration::from_secs(5), plain) => refused,
			request = server.accept() => panic!("plaintext client yielded a request: {}", request.is_some()),
		};
		assert!(refused.expect("plaintext client was not refused").unwrap());

		rtmps_publish(&mut server, connector).await;
	}

	/// Refusing plaintext with no TLS to serve instead would listen for nothing, so the
	/// server refuses to run rather than dropping every client.
	#[tokio::test]
	async fn plaintext_off_without_tls_is_refused() {
		let mut server = Server::bind("127.0.0.1:0".parse().unwrap())
			.await
			.unwrap()
			.with_plaintext(false);

		assert!(server.check().is_err());
		assert!(server.accept().await.is_none());
	}
}
