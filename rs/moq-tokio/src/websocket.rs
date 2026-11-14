//! WebSocket fallback transport, running the QMux wire format over `ws://` or `wss://`.
//!
//! Used when QUIC is unreachable: UDP blocked by a firewall, a proxy in the way, a
//! network that only passes TCP/443. The client races this against QUIC and gives QUIC
//! a small head start ([`Config::delay`]), so WebSocket only wins when QUIC can't get
//! through. A reconnecting [`Connection`](crate::Connection) that lands on WebSocket
//! keeps probing QUIC on a slow backoff and moves back once a probe connects, handing
//! over make-before-break like a GOAWAY migration. Servers accept it on a separate TCP
//! port via [`Listener`], or pass an HTTP server's upgraded socket to [`accept`] before
//! admitting the returned request.

use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::{net, time};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, http};
use url::Url;

use crate::cli::Duration as CliDuration;
/// Errors specific to the WebSocket fallback backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Every candidate failed its TCP, TLS, or WebSocket handshake.
	#[error("all {} connection attempts failed: {}", .0.len(), crate::failover::describe(.0))]
	Failover(Vec<crate::failover::Attempt<Error>>),

	/// The TCP socket failed to bind or connect. Not accept: a failed `accept(2)` is
	/// the listener's own to classify and retry (see [`crate::accept`]).
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// WebSocket fallback was turned off via [`Config::enabled`].
	#[error("WebSocket support is disabled")]
	Disabled,

	/// The URL had no host to dial.
	#[error("missing hostname")]
	MissingHostname,

	/// The TLS server name could not be used.
	#[error(transparent)]
	Tls(#[from] crate::tls::Error),

	/// The URL scheme can't carry WebSocket. Only `http`, `https`, `ws`, and `wss` work.
	#[error("unsupported URL scheme for WebSocket: {0}")]
	UnsupportedScheme(String),

	/// The qmux handshake failed while dialing, including a non-101 upgrade response
	/// from the server.
	#[error("failed to connect WebSocket: {message}")]
	Connect {
		/// What qmux reported.
		message: String,
		/// The HTTP status the server answered the upgrade with, when it answered with one.
		///
		/// Read at conversion time rather than kept as a qmux error, so the classification
		/// survives without qmux appearing in this crate's public API.
		status: Option<u16>,
	},

	/// The URL couldn't be turned into a valid WebSocket handshake request.
	#[error("failed to build WebSocket request: {0}")]
	BuildRequest(String),

	/// An ALPN contained bytes that aren't legal in the `Sec-WebSocket-Protocol` header.
	#[error("failed to build WebSocket protocols header: {0}")]
	ProtocolHeader(String),

	/// The TCP/TLS connection or the WebSocket upgrade itself failed.
	#[error("failed to connect WebSocket: {0}")]
	WebSocketConnect(String),

	/// The server refused the connection outright, so retrying won't help.
	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

	/// The qmux handshake failed while accepting an incoming connection.
	#[error("WebSocket accept failed: {0}")]
	Accept(String),
}

impl crate::failover::Aggregate for Error {
	fn aggregate(failures: Vec<crate::failover::Attempt<Self>>) -> Self {
		Self::Failover(failures)
	}
	fn resolve(error: Option<std::io::Error>) -> Self {
		Self::Io(error.unwrap_or_else(|| std::io::Error::other("no fixed addresses")))
	}
}

impl Error {
	/// A failed dial, keeping the upgrade status qmux reported.
	fn connect(err: qmux::Error) -> Self {
		let status = match err {
			qmux::Error::Http(status) => Some(status),
			_ => None,
		};

		Self::Connect {
			message: crate::error::message(err),
			status,
		}
	}

	/// A failed accept.
	fn accept(err: qmux::Error) -> Self {
		Self::Accept(crate::error::message(err))
	}
}

type Result<T> = std::result::Result<T, Error>;

/// The MoQ handshake policy and connection facts for an upgraded WebSocket.
#[non_exhaustive]
pub struct Options {
	pub(crate) url: Url,
	pub(crate) server: moq_net::Server,
	pub(crate) active: moq_net::server::Admitted,
	/// The addresses and negotiated subprotocol captured before the upgrade.
	pub link: crate::server::Link,
	/// The verified TLS peer, when the HTTP server requested a certificate.
	pub identity: Option<crate::tls::PeerIdentity>,
	/// The socket measurements captured before the HTTP server erased its descriptor.
	pub socket_stats: Option<qmux::SharedSocketStats>,
	/// The `[quic]` windows and stream count the session starts with, until
	/// [`crate::server::Request::with_quic_limits`] widens it. Defaults to the `[quic]`
	/// defaults, so a server passes its own resolved section.
	pub quic: crate::quic::Resolved,
}

impl Options {
	/// Carry an accepted connection's guard, URL, and MoQ policy through its upgrade.
	pub fn new(url: Url, server: moq_net::Server, active: moq_net::server::Admitted) -> Self {
		Self {
			url,
			server,
			active,
			link: Default::default(),
			identity: None,
			socket_stats: None,
			quic: Default::default(),
		}
	}
}

/// Read SETUP on a connection an HTTP server upgraded to WebSocket, leaving admission
/// to the returned request.
///
/// `io` is the raw upgraded connection, past the 101 response. The accept deadline runs
/// from here, set by `options`' server ([`moq_net::Server::with_timeout`]).
pub async fn accept<T>(io: T, options: Options) -> crate::Result<crate::server::Request>
where
	T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
	crate::runtime::scope(async move {
		let deadline = options.server.deadline(kio::clock::now());
		// A vanished peer sends no FIN. Ping and timeout bound the same stale-session
		// lifetime that QUIC's idle timeout bounds on the native path.
		// The subprotocol names the QMux version; an upgrade without one has no wire format.
		let alpn = options.link.alpn.as_deref().unwrap_or_default();
		let mut upgraded = qmux::ws::Upgraded::new(io, alpn)
			.with_keep_alive(qmux::ws::KeepAlive::default())
			.with_session_config(crate::qmux_config(&options.quic));
		if let Some(stats) = &options.socket_stats {
			upgraded = upgraded.with_socket_stats(stats.clone());
		}
		// The deadline covers QMux establishment too, which waits for the peer's parameters.
		let accepted = async { upgraded.accept().await.map_err(Error::accept) };
		let session = crate::spawn_qmux(crate::server::within(deadline, accepted).await?);
		crate::server::accept_websocket(session, options, deadline).await
	})
	.await
}

/// Servers (host, port) where WebSocket won the race, so later dials skip QUIC's head start.
///
/// Both arms still race on those dials. The entry is provisional: a QUIC win or probe
/// clears it through [`forget`], since UDP gets through after all, and so does the end of
/// the WebSocket session that won, so a redial after an outage gives QUIC its head start.
static WEBSOCKET_WON: LazyLock<Mutex<HashSet<(String, u16)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// The [`WEBSOCKET_WON`] key for a dial URL: the host and port the fallback dials.
fn won_key(url: &Url) -> Result<(String, u16)> {
	let host = url.host_str().ok_or(Error::MissingHostname)?.to_string();
	let port = url.port().unwrap_or_else(|| match url.scheme() {
		"https" | "wss" | "moql" | "moqt" => 443,
		"http" | "ws" => 80,
		_ => 443,
	});
	Ok((host, port))
}

/// Forget that WebSocket won for `url`, giving QUIC its head start again.
#[cfg(feature = "quic")]
pub(crate) fn forget(url: &Url) {
	if let Ok(key) = won_key(url) {
		WEBSOCKET_WON.lock().unwrap().remove(&key);
	}
}

/// Record that WebSocket won for `url`, as a fallback dial that lands does.
#[cfg(all(test, feature = "quic"))]
pub(crate) fn remember(url: &Url) {
	WEBSOCKET_WON.lock().unwrap().insert(won_key(url).unwrap());
}

/// Whether the next dial to `url` skips QUIC's head start.
#[cfg(all(test, feature = "quic"))]
pub(crate) fn won(url: &Url) -> bool {
	won_key(url).is_ok_and(|key| WEBSOCKET_WON.lock().unwrap().contains(&key))
}

/// WebSocket configuration for the client.
#[derive(Clone, Debug, Default, usage_rs::Args, serde::Serialize, serde::Deserialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Config {
	/// Whether to enable the WebSocket fallback. Defaults to true.
	///
	/// `None` means the default (on). Distinct from `Some(false)`, which turns it off.
	#[usage(
		name = "connect-websocket-enabled",
		long = "connect-websocket-enabled",
		env = "MOQ_CONNECT_WEBSOCKET_ENABLED",
		setting = "connect.websocket.enabled",
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub enabled: Option<bool>,

	/// Head start given to the QUIC dial before the WebSocket fallback joins the
	/// race. Defaults to 200ms, and drops to zero for a server WebSocket already won.
	/// `None` uses the default; zero disables this duration.
	#[usage(
		name = "connect-websocket-delay",
		long = "connect-websocket-delay",
		env = "MOQ_CONNECT_WEBSOCKET_DELAY",
		setting = "connect.websocket.delay"
	)]
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub delay: Option<crate::cli::Duration>,

	/// The released `MOQ_CLIENT_WEBSOCKET_*` env vars, named by [`Config::deprecated`].
	#[usage(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,
}

/// The released spellings for this section, which moved to `--connect-websocket-*`
/// and `MOQ_CONNECT_WEBSOCKET_*` with the rest of the dial side.
///
/// Separate args rather than Usage aliases, since an alias renames the flag but
/// leaves its env var behind.
#[derive(Clone, Debug, Default, usage_rs::Args)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub(crate) struct Legacy {
	#[usage(
		name = "websocket-enabled",
		long = "websocket-enabled",
		env = "MOQ_CLIENT_WEBSOCKET_ENABLED",
		hide = true,
		default_missing = "true",
		num_args = 0..=1,
		require_equals = true,
	)]
	enabled: Option<bool>,

	#[usage(
		name = "websocket-delay",
		long = "websocket-delay",
		env = "MOQ_CLIENT_WEBSOCKET_DELAY",
		hide = true
	)]
	delay: Option<CliDuration>,
}

/// The QUIC head start when `--connect-websocket-delay` is unset.
const DEFAULT_DELAY: time::Duration = time::Duration::from_millis(200);

impl Config {
	/// The released spellings in use, each paired with what replaced it. Reached
	/// through [`crate::connect::Config::deprecated`].
	pub(crate) fn deprecated(&self) -> crate::cli::Deprecated {
		let mut found = crate::cli::Deprecated::default();
		if self.legacy.enabled.is_some() {
			found.flag(
				"--websocket-enabled",
				Some("MOQ_CLIENT_WEBSOCKET_ENABLED"),
				"--connect-websocket-enabled / MOQ_CONNECT_WEBSOCKET_ENABLED",
			);
		}
		if self.legacy.delay.is_some() {
			found.flag(
				"--websocket-delay",
				Some("MOQ_CLIENT_WEBSOCKET_DELAY"),
				"--connect-websocket-delay / MOQ_CONNECT_WEBSOCKET_DELAY",
			);
		}
		found
	}

	/// The fallback knobs with defaults applied, ready to run the race with.
	pub fn resolve(&self) -> Resolved {
		Resolved {
			enabled: self.enabled.unwrap_or(true),
			delay: self.delay.map(Into::into).unwrap_or(DEFAULT_DELAY),
		}
	}
}

/// The fallback knobs with defaults filled in, produced by [`Config::resolve`].
///
/// Non-exhaustive because it gains a field for every knob [`Config`] gains, so build
/// it from [`Config::resolve`] rather than a struct literal.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Resolved {
	/// Whether the fallback runs at all.
	pub enabled: bool,
	/// The head start a QUIC dial gets before the fallback joins the race.
	pub delay: time::Duration,
}

/// The fallback arm of the QUIC-vs-WebSocket race, so only compiled when there is a
/// QUIC dial to race against. A WebSocket-only build calls [`connect`] directly.
#[cfg(feature = "quic")]
pub(crate) async fn race_handle(
	config: &Config,
	tls: &rustls::ClientConfig,
	tls_host_name: Option<&str>,
	addr: crate::connect::Addr,
	alpns: &[&str],
	session: &qmux::Config,
) -> Option<Result<qmux::Session>> {
	if !config.resolve().enabled {
		return None;
	}

	let url = addr.url();
	// Only attempt WebSocket for HTTP-based schemes.
	// Custom protocols (moqt://, moql://) use raw QUIC and don't support WebSocket.
	match url.scheme() {
		"http" | "https" | "ws" | "wss" => {}
		_ => return None,
	}

	let res = connect(config, tls, tls_host_name, addr, alpns, session).await;
	if let Err(err) = &res {
		tracing::warn!(%err, "WebSocket connection failed");
	}
	Some(res)
}

pub(crate) async fn connect(
	config: &Config,
	tls: &rustls::ClientConfig,
	tls_host_name: Option<&str>,
	addr: crate::connect::Addr,
	alpns: &[&str],
	session: &qmux::Config,
) -> Result<qmux::Session> {
	let mut url = addr.url().clone();
	let resolved = config.resolve();
	if !resolved.enabled {
		return Err(Error::Disabled);
	}

	// Convert URL scheme: http:// -> ws://, https:// -> wss://
	// Custom protocols (moqt://, moql://) use raw QUIC and don't support WebSocket.
	let needs_tls = match url.scheme() {
		"http" => {
			url.set_scheme("ws").expect("failed to set scheme");
			false
		}
		"https" => {
			url.set_scheme("wss").expect("failed to set scheme");
			true
		}
		"ws" => false,
		"wss" => true,
		_ => return Err(Error::UnsupportedScheme(url.scheme().to_string())),
	};

	let key = won_key(&url)?;
	let (host, port) = key.clone();

	// A secure dial presents this name on every attempt; the override was checked when the client was built.
	if needs_tls && tls_host_name.is_none() {
		match url.host() {
			// URL brackets delimit IPv6 literals, but aren't part of the TLS server name.
			Some(url::Host::Ipv6(ip)) => crate::tls::server_name(&ip.to_string())?,
			_ => crate::tls::server_name(&host)?,
		}
	}

	// Apply a small penalty to WebSocket to improve odds for QUIC to connect first,
	// unless we've already had to fall back to WebSockets for this server.
	// TODO if let chain
	match resolved.delay {
		delay if !delay.is_zero() && !WEBSOCKET_WON.lock().unwrap().contains(&key) => {
			kio::clock::sleep(delay).await;
			tracing::debug!(peer = %crate::connect::Endpoint(&url), delay_ms = %delay.as_millis(), "QUIC not yet connected, attempting WebSocket fallback");
		}
		_ => {}
	}

	tracing::debug!(peer = %crate::connect::Endpoint(&url), "connecting via WebSocket");

	let session = match (needs_tls, tls_host_name, addr.addresses()) {
		(true, name, addrs) if name.is_some() || addrs.is_some() => {
			let candidates = match addrs {
				Some(addrs) => crate::resolve::Candidates::fixed(addrs.iter().copied()),
				None => crate::resolve::Candidates::resolve(
					url.host().ok_or(Error::MissingHostname)?,
					port,
					crate::connect::DEFAULT_RESOLUTION_DELAY,
				),
			};
			let name = name.unwrap_or(&host);
			crate::failover::race(candidates, crate::connect::DEFAULT_RACE, |addr| {
				let url = &url;
				async move {
					let stream = tokio::net::TcpStream::connect(addr).await?;
					stream.set_nodelay(true)?;
					connect_tls_override(tls, name, url, stream, alpns, session).await
				}
			})
			.await?
		}
		_ => {
			// Most moq ALPNs can ride on any QMux draft (`&[]` lets the polyfill expand
			// to every version it knows). `qmux_versions_for` pins the few that the spec
			// restricts. qmux also offers the bare ALPNs (`qmux-02`, `qmux-01`,
			// `qmux-00`) by default so we still interop with relays that only know a
			// wire-format version.
			// The existing TLS config (which respects tls-disable-verify) for secure connections.
			qmux::ws::Client::new()
				.with_protocols(alpns.iter().map(|&a| (a, qmux_versions_for(a))))
				.with_tls(Arc::new(tls.clone()))
				.with_keep_alive(qmux::ws::KeepAlive::default()) // 5s ping / 30s deadline; TCP backs off retransmits too far for QUIC's 10s
				.with_session_config(session.clone())
				.connect(url.as_str())
				.await
				.map(crate::spawn_qmux)
				.map_err(Error::connect)?
		}
	};

	tracing::warn!(peer = %crate::connect::Endpoint(&url), "using WebSocket fallback");
	WEBSOCKET_WON.lock().unwrap().insert(key);

	Ok(session)
}

/// Dial the URL address while using a different server name for the TLS layer.
async fn connect_tls_override(
	tls: &rustls::ClientConfig,
	tls_host_name: &str,
	url: &Url,
	stream: tokio::net::TcpStream,
	alpns: &[&str],
	session: &qmux::Config,
) -> Result<qmux::Session> {
	let mut request = url
		.as_str()
		.into_client_request()
		.map_err(|err| Error::BuildRequest(crate::error::message(err)))?;
	let protocols = supported_subprotocols(alpns).join(", ");
	request.headers_mut().insert(
		http::header::SEC_WEBSOCKET_PROTOCOL,
		http::HeaderValue::from_str(&protocols).map_err(|err| Error::ProtocolHeader(crate::error::message(err)))?,
	);

	// URL brackets delimit IPv6 literals, but aren't part of the TLS server name.
	let bare = tls_host_name.trim_start_matches('[').trim_end_matches(']');
	let name = rustls::pki_types::ServerName::try_from(bare.to_string())
		.map_err(|_| Error::BuildRequest(format!("invalid TLS host name: {tls_host_name}")))?;
	let mut stream = tokio_rustls::TlsConnector::from(Arc::new(tls.clone()))
		.connect(name, stream)
		.await
		.map_err(|err| Error::WebSocketConnect(crate::error::message(err)))?;
	let negotiated = upgrade(&mut stream, &request).await?;

	qmux::ws::Upgraded::new(stream, &negotiated)
		.with_keep_alive(qmux::ws::KeepAlive::default())
		.with_session_config(session.clone())
		.connect()
		.await
		.map(crate::spawn_qmux)
		.map_err(Error::connect)
}

/// The most response header bytes [`upgrade`] reads before giving up on the server.
const MAX_RESPONSE: usize = 16 * 1024;

/// Send the WebSocket upgrade `request` on `stream` and read the 101 that answers it,
/// returning the subprotocol the server chose from those the request offered.
///
/// The response is read a byte at a time so nothing past it is consumed: the server may
/// send QMux frames right behind its 101, and those belong to the session.
async fn upgrade<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S, request: &http::Request<()>) -> Result<String> {
	let key = request
		.headers()
		.get(http::header::SEC_WEBSOCKET_KEY)
		.expect("a client request carries its key")
		.clone();
	let path = request.uri().path_and_query().map_or("/", |path| path.as_str());
	let mut head = format!("GET {path} HTTP/1.1\r\n").into_bytes();
	for (name, value) in request.headers() {
		head.extend_from_slice(name.as_str().as_bytes());
		head.extend_from_slice(b": ");
		head.extend_from_slice(value.as_bytes());
		head.extend_from_slice(b"\r\n");
	}
	head.extend_from_slice(b"\r\n");
	stream.write_all(&head).await?;
	stream.flush().await?;

	let mut response = Vec::new();
	while !response.ends_with(b"\r\n\r\n") {
		if response.len() == MAX_RESPONSE {
			return Err(Error::WebSocketConnect("upgrade response headers too large".into()));
		}
		response.push(stream.read_u8().await?);
	}

	let mut headers = [httparse::EMPTY_HEADER; tungstenite::handshake::headers::MAX_HEADERS];
	let mut parsed = httparse::Response::new(&mut headers);
	parsed
		.parse(&response)
		.map_err(|err| Error::WebSocketConnect(format!("invalid upgrade response: {err}")))?;
	let status = parsed.code.unwrap_or_default();
	if status != 101 {
		return Err(Error::Connect {
			message: format!("upgrade refused with HTTP {status}"),
			status: Some(status),
		});
	}
	let invalid = |reason: &str| Err(Error::WebSocketConnect(format!("invalid upgrade response: {reason}")));
	if parsed.version != Some(1) {
		return invalid("not HTTP/1.1");
	}
	let header = |name: http::header::HeaderName| {
		parsed
			.headers
			.iter()
			.find(|header| header.name.eq_ignore_ascii_case(name.as_str()))
			.and_then(|header| std::str::from_utf8(header.value).ok())
	};
	if !header(http::header::UPGRADE).is_some_and(|value| value.trim().eq_ignore_ascii_case("websocket")) {
		return invalid("no Upgrade: websocket");
	}
	// `Connection` may arrive as several fields; the upgrade token can sit in any of them.
	let upgrades = parsed
		.headers
		.iter()
		.filter(|header| header.name.eq_ignore_ascii_case(http::header::CONNECTION.as_str()))
		.filter_map(|header| std::str::from_utf8(header.value).ok())
		.flat_map(|value| value.split(','))
		.any(|token| token.trim().eq_ignore_ascii_case("upgrade"));
	if !upgrades {
		return invalid("no Connection: upgrade");
	}
	let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
	if header(http::header::SEC_WEBSOCKET_ACCEPT) != Some(accept.as_str()) {
		return invalid("wrong accept key");
	}

	// The subprotocol pairs the QMux draft with the moq version, so one the request never
	// offered could pair them in a way we refuse, such as moqt-22 over qmux-00.
	let offered = request
		.headers()
		.get(http::header::SEC_WEBSOCKET_PROTOCOL)
		.and_then(|value| value.to_str().ok())
		.unwrap_or_default();
	match header(http::header::SEC_WEBSOCKET_PROTOCOL).map(str::trim) {
		Some(protocol) if offered.split(',').any(|offer| offer.trim() == protocol) => Ok(protocol.to_string()),
		Some(_) => invalid("subprotocol was not offered"),
		None => invalid("no subprotocol"),
	}
}

/// The QMux drafts a moq ALPN is allowed to ride on, for `qmux::ws::Client::with_protocols`.
///
/// moq-transport-18 and newer require qmux-01, so we never pair them with qmux-00.
/// This mirrors the policy in `js/net`'s `connect.ts`. Every other ALPN returns
/// `&[]`, which qmux expands to every draft it knows about.
const QMUX01_ONLY_ALPNS: &[&str] = &["moqt-18", "moqt-20", "moqt-22"];

fn qmux_versions_for(alpn: &str) -> &'static [qmux::Version] {
	if QMUX01_ONLY_ALPNS.contains(&alpn) {
		&[qmux::Version::QMux01]
	} else {
		&[]
	}
}

impl Error {
	pub(crate) fn connect_error(&self) -> Option<crate::ConnectError> {
		match self {
			Self::ConnectRejected(err) => Some(*err),
			Self::Failover(failures) => failures.iter().find_map(|failure| failure.error.connect_error()),
			// qmux surfaces a non-101 WebSocket upgrade response as a status;
			// map an auth rejection (401/403) so the caller sees it as terminal.
			Self::Connect {
				status: Some(status), ..
			} => crate::ConnectError::from_status_u16(*status),
			_ => None,
		}
	}

	/// The HTTP status the server answered the upgrade with, if it answered with one at all.
	///
	/// qmux surfaces a non-101 WebSocket upgrade response as `Http(status)`. See
	/// [`crate::Error::status`].
	pub(crate) fn status(&self) -> Option<u16> {
		match self {
			Self::Connect { status, .. } => *status,
			Self::Failover(failures) => {
				let mut settled = None;
				for failure in failures {
					match failure.error.status() {
						Some(status) if !crate::error::status_retryable(status) => settled = Some(status),
						_ => return None,
					}
				}
				settled
			}
			_ => None,
		}
	}
}

/// Listens for incoming WebSocket connections on a TCP port.
///
/// Assign to [`crate::server::Config::websocket`] to accept WebSocket connections
/// alongside QUIC connections on a separate port.
pub struct Listener {
	listener: tokio::net::TcpListener,
	protocols: Vec<String>,
	health: crate::accept::Health,
	/// The settings each accepted qmux session starts with.
	session: qmux::Config,
}

impl Listener {
	/// Bind a listener to the given address, accepting every moq ALPN we know about.
	pub async fn bind(addr: net::SocketAddr) -> Result<Self> {
		crate::runtime::scope(async move {
			let listener = tokio::net::TcpListener::bind(addr).await?;
			let protocols = supported_subprotocols(moq_net::ALPNS);
			Ok(Self {
				listener,
				protocols,
				health: crate::accept::Health::new("websocket"),
				session: crate::qmux_default(),
			})
		})
		.await
	}

	/// Accept only the given moq ALPNs, in preference order.
	pub fn with_protocols<I, S>(mut self, protocols: I) -> Result<Self>
	where
		I: IntoIterator<Item = S>,
		S: AsRef<str>,
	{
		let alpns: Vec<String> = protocols
			.into_iter()
			.map(|protocol| protocol.as_ref().to_owned())
			.collect();
		let refs: Vec<&str> = alpns.iter().map(String::as_str).collect();
		let protocols = supported_subprotocols(&refs);
		for protocol in &protocols {
			http::HeaderValue::from_str(protocol).map_err(|err| Error::ProtocolHeader(crate::error::message(err)))?;
		}
		self.protocols = protocols;
		Ok(self)
	}

	/// Open each accepted session with `quic`'s windows and stream count, the `[quic]`
	/// section, rather than its defaults. A [`crate::Server`] applies its own.
	pub fn with_quic(mut self, quic: &crate::quic::Resolved) -> Self {
		self.session = crate::qmux_config(quic);
		self
	}

	/// The local address the listener is bound to.
	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	/// A live handle to this listener's accept-loop health, for an embedder that
	/// publishes it (see [`crate::accept`]).
	pub fn accept_health(&self) -> crate::accept::Health {
		self.health.clone()
	}

	/// Accept the next connection, performing the WebSocket upgrade and qmux handshake.
	///
	/// A failed `accept(2)` is handled here rather than yielded: it is classified,
	/// counted, logged, and paced by [`accept_health`](Self::accept_health), then
	/// retried, because the caller has no better answer than to ask again. A
	/// per-connection upgrade failure is still returned as an error.
	pub async fn accept(&self) -> Result<qmux::Session> {
		crate::runtime::scope(async move { self.accept_with_url().await.map(|(session, _, _)| session) }).await
	}

	/// Accept the next connection and retain the WebSocket request URL, the chosen
	/// sub-protocol, and the peer's address.
	pub(crate) async fn accept_with_url(&self) -> Result<(qmux::Session, Url, Accepted)> {
		self.accept_pending().await.await
	}

	/// Accept a socket and return its independently driven WebSocket upgrade, so a
	/// peer that stalls mid-upgrade holds up only itself.
	pub(crate) async fn accept_pending(
		&self,
	) -> impl Future<Output = Result<(qmux::Session, Url, Accepted)>> + Send + use<> {
		let (stream, addr) = self.accept_socket().await;
		tracing::debug!(%addr, "accepted WebSocket TCP connection");

		let accepted = Arc::new(Mutex::new(None::<(String, Url)>));
		let accepted_callback = accepted.clone();
		let protocols = self.protocols.clone();
		let session = self.session.clone();
		#[expect(
			clippy::result_large_err,
			reason = "the error type is tungstenite's handshake response, not ours"
		)]
		let callback = move |request: &tungstenite::handshake::server::Request,
		               mut response: tungstenite::handshake::server::Response|
		      -> std::result::Result<_, tungstenite::handshake::server::ErrorResponse> {
			let offered: Vec<_> = request
				.headers()
				.get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
				.iter()
				.filter_map(|value| value.to_str().ok())
				.flat_map(|value| value.split(','))
				.map(str::trim)
				.filter(|value| !value.is_empty())
				.collect();
			let Some(protocol) = select_subprotocol(&offered, &protocols) else {
				return Err(http::Response::builder()
					.status(http::StatusCode::BAD_REQUEST)
					.body(Some("no supported protocol".to_string()))
					.expect("valid rejection response"));
			};
			let Some(url) = websocket_request_url(request) else {
				return Err(http::Response::builder()
					.status(http::StatusCode::BAD_REQUEST)
					.body(Some("invalid request URL".to_string()))
					.expect("valid rejection response"));
			};

			response.headers_mut().insert(
				http::header::SEC_WEBSOCKET_PROTOCOL,
				http::HeaderValue::from_str(protocol).expect("protocol validated at bind"),
			);
			*accepted_callback.lock().unwrap() = Some((protocol.to_string(), url));
			Ok(response)
		};

		async move {
			// The client waits for the 101 before it sends a frame, so the handshake reads
			// nothing past the request and the stream underneath holds only frames.
			let stream = tokio_tungstenite::accept_hdr_async(stream, callback)
				.await
				.map_err(|err| Error::Accept(crate::error::message(err)))?
				.into_inner();
			let (protocol, url) = accepted
				.lock()
				.unwrap()
				.take()
				.expect("successful upgrade selected a protocol");
			let session = qmux::ws::Upgraded::new(stream, &protocol)
				.with_keep_alive(qmux::ws::KeepAlive::default())
				.with_session_config(session)
				.accept()
				.await
				.map(crate::spawn_qmux)
				.map_err(Error::accept)?;
			Ok((session, url, Accepted { remote: addr, protocol }))
		}
	}

	/// The `accept(2)` half: keep asking until a connection comes back.
	async fn accept_socket(&self) -> (tokio::net::TcpStream, net::SocketAddr) {
		loop {
			match self.listener.accept().await {
				Ok((stream, addr)) => {
					// qmux already batches records, so Nagle would only hold a batch's
					// tail for the peer's delayed ACK. Only a reset socket refuses it.
					if let Err(err) = stream.set_nodelay(true) {
						tracing::debug!(%addr, %err, "dropped a connection that refused TCP_NODELAY");
						continue;
					}
					self.health.accepted();
					return (stream, addr);
				}
				Err(err) => {
					if let Some(delay) = self.health.failed(&err) {
						kio::clock::sleep(delay).await;
					}
				}
			}
		}
	}
}

/// What the upgrade learned about the peer, beside the session and URL.
pub(crate) struct Accepted {
	/// The peer's socket address.
	pub remote: net::SocketAddr,
	/// The sub-protocol the upgrade selected.
	pub protocol: String,
}

/// The first supported subprotocol the client offered. One that offers none names no QMux
/// version, so there is no wire format to speak.
fn select_subprotocol<'a>(offered: &[&str], supported: &'a [String]) -> Option<&'a str> {
	supported
		.iter()
		.find(|protocol| offered.contains(&protocol.as_str()))
		.map(String::as_str)
}

/// Reconstruct the client request URL from an absolute URI or the HTTP Host header.
fn websocket_request_url(request: &tungstenite::handshake::server::Request) -> Option<Url> {
	let uri = request.uri();
	if uri.scheme().is_some() && uri.authority().is_some() {
		return Url::parse(&uri.to_string()).ok();
	}

	let host = request.headers().get(http::header::HOST)?.to_str().ok()?;
	Url::parse(&format!("ws://{host}{uri}")).ok()
}

/// WebSocket subprotocols accepted for the given MoQ ALPNs, in preference order.
fn supported_subprotocols(alpns: &[&str]) -> Vec<String> {
	let mut protocols = Vec::new();
	for &alpn in alpns {
		let versions = qmux_versions_for(alpn);
		let versions = if versions.is_empty() {
			qmux::Version::ALL
		} else {
			versions
		};
		protocols.extend(
			versions
				.iter()
				.copied()
				.map(|version| format!("{}{alpn}", version.prefix())),
		);
	}
	protocols.extend(qmux::Version::ALL.iter().map(|version| version.alpn().to_string()));
	protocols
}

#[cfg(test)]
mod tests {
	use super::*;
	use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

	#[test]
	fn subprotocol_selection_requires_an_offer() {
		let supported = vec!["qmux-01.moq-lite-05".to_string()];
		assert_eq!(select_subprotocol(&[], &supported), None);
		assert_eq!(
			select_subprotocol(&["qmux-01.moq-lite-05"], &supported),
			Some("qmux-01.moq-lite-05")
		);
		assert_eq!(select_subprotocol(&["webtransport"], &supported), None);
	}

	/// A client offering no subprotocol names no QMux version, so the upgrade is refused
	/// rather than falling back to the retired `webtransport` wire format.
	#[tokio::test]
	async fn listener_refuses_a_client_without_subprotocol() {
		let listener = Listener::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = listener.local_addr().unwrap();
		let accepted = tokio::spawn(async move { listener.accept_with_url().await });

		let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
		let err = tokio_tungstenite::client_async(format!("ws://{addr}/room"), stream)
			.await
			.unwrap_err();
		assert!(
			matches!(&err, tungstenite::Error::Http(response) if response.status() == http::StatusCode::BAD_REQUEST),
			"got {err:?}"
		);
		assert!(accepted.await.unwrap().is_err());
	}

	#[tokio::test]
	async fn listener_disables_nagle() {
		let listener = Listener::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = listener.local_addr().unwrap();
		let _client = tokio::net::TcpStream::connect(addr).await.unwrap();
		let (stream, _) = listener.accept_socket().await;
		assert!(stream.nodelay().unwrap());
	}

	#[tokio::test]
	async fn tls_host_name_override_dials_url_address() {
		kio::clock::host_time(); // real TLS over loopback
		check_tls_authority("127.0.0.1", false, Some("relay.example")).await;
	}

	#[tokio::test]
	async fn fixed_addresses_keep_tls_name_and_request_host() {
		kio::clock::host_time(); // real TLS over loopback
		check_tls_authority("relay.example", true, None).await;
	}

	#[tokio::test]
	async fn ipv6_literal() {
		kio::clock::host_time(); // real TLS over loopback
		check_tls_authority("[::1]", false, None).await;
	}

	#[tokio::test]
	async fn ipv6_literal_fixed_addresses() {
		kio::clock::host_time(); // real TLS over loopback
		check_tls_authority("[::1]", true, None).await;
	}

	#[tokio::test]
	async fn ipv6_tls_host_name_override() {
		kio::clock::host_time(); // real TLS over loopback
		check_tls_authority("[::1]", false, Some("::1")).await;
	}

	/// Dial `wss://{url_host}` on loopback, optionally pinned to fixed addresses, and
	/// check the TLS name and HTTP `Host` the server sees.
	///
	/// Stays on the wall clock. A paused clock auto-advances while the real dial
	/// waits, so a timer on this path can fire before loopback delivers.
	async fn check_tls_authority(url_host: &str, fixed: bool, tls_name: Option<&str>) {
		let ipv6 = url_host.starts_with('[');
		let name = tls_name.unwrap_or(url_host.trim_start_matches('[').trim_end_matches(']'));
		// Clients send SNI only for DNS names, never for IP addresses.
		let expected_sni = name.parse::<net::IpAddr>().is_err().then_some(name);

		let rcgen::CertifiedKey { cert, signing_key } = (|| -> std::result::Result<_, rcgen::Error> {
			let signing_key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256)?;
			let mut params = rcgen::CertificateParams::new([name.to_string()])?;
			params.serial_number = Some(1u64.into());
			let cert = params.self_signed(&signing_key)?;
			Ok(rcgen::CertifiedKey { cert, signing_key })
		})()
		.unwrap();
		let cert = CertificateDer::from(cert);
		let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
			signing_key.to_pkcs8().secret_pkcs8_der().to_vec(),
		));
		let provider = crate::crypto::provider().unwrap();
		let server_tls = rustls::ServerConfig::builder_with_provider(provider.clone())
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_no_client_auth()
			.with_single_cert(vec![cert.clone()], key)
			.unwrap();

		let mut roots = rustls::RootCertStore::empty();
		roots.add(cert).unwrap();
		let client_tls = rustls::ClientConfig::builder_with_provider(provider)
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_root_certificates(roots)
			.with_no_client_auth();

		let loopback = if ipv6 { "[::1]:0" } else { "127.0.0.1:0" };
		let listener = tokio::net::TcpListener::bind(loopback).await.unwrap();
		let addr = listener.local_addr().unwrap();
		let accepted = tokio::spawn(async move {
			let (stream, _) = listener.accept().await.unwrap();
			let tls = tokio_rustls::TlsAcceptor::from(Arc::new(server_tls))
				.accept(stream)
				.await
				.unwrap();
			let server_name = tls.get_ref().1.server_name().map(str::to_string);
			let host = Arc::new(Mutex::new(None));
			let request_host = host.clone();
			let protocol = Arc::new(Mutex::new(None));
			let selected = protocol.clone();
			#[expect(
				clippy::result_large_err,
				reason = "the error type is tungstenite's handshake response, not ours"
			)]
			let callback = move |request: &tungstenite::handshake::server::Request,
			            mut response: tungstenite::handshake::server::Response| {
				*request_host.lock().unwrap() = request
					.headers()
					.get(http::header::HOST)
					.and_then(|value| value.to_str().ok())
					.map(str::to_string);
				if let Some(protocol) = request
					.headers()
					.get(http::header::SEC_WEBSOCKET_PROTOCOL)
					.and_then(|value| value.to_str().ok())
					.and_then(|value| value.split(',').next())
					.map(str::trim)
				{
					response.headers_mut().insert(
						http::header::SEC_WEBSOCKET_PROTOCOL,
						http::HeaderValue::from_str(protocol).unwrap(),
					);
					*selected.lock().unwrap() = Some(protocol.to_string());
				}
				Ok(response)
			};
			let tls = tokio_tungstenite::accept_hdr_async(tls, callback)
				.await
				.unwrap()
				.into_inner();
			let protocol = protocol.lock().unwrap().take().unwrap();
			// The client returns once QMux is established, so answer with its parameters.
			let (session, driver) = qmux::ws::Upgraded::new(tls, &protocol).accept().await.unwrap();
			tokio::spawn(driver);
			let host = host.lock().unwrap().take();
			(server_name, host, session)
		});

		let config = Config::default();
		let url = Url::parse(&format!("wss://{url_host}:{}/anon", addr.port())).unwrap();
		// A TCP-only race would select this silent TLS peer and strand the dial.
		let silent = tokio::net::TcpListener::bind(loopback).await.unwrap();
		let target = if fixed {
			crate::connect::Addr::pinned(url, [silent.local_addr().unwrap(), addr]).unwrap()
		} else {
			url.into()
		};
		let expected_host = format!("{url_host}:{}", addr.port());
		let session = connect(
			&config,
			&client_tls,
			tls_name,
			target,
			moq_net::ALPNS,
			&crate::qmux_default(),
		)
		.await
		.unwrap();
		drop(session);
		let (server_name, host, _session) = accepted.await.unwrap();
		assert_eq!(server_name.as_deref(), expected_sni);
		assert_eq!(host.as_deref(), Some(expected_host.as_str()));
	}

	/// A session [`accept`] upgrades, as the relay's own WebSocket listener does, starts at
	/// the `[quic]` windows its [`Options`] carry rather than the defaults.
	#[tokio::test]
	async fn an_upgraded_session_starts_at_the_options_windows() {
		const TIMEOUT: time::Duration = time::Duration::from_secs(10);

		let alpn = supported_subprotocols(moq_net::ALPNS).remove(0);
		let (server_ws, client_ws) = tokio::io::duplex(1 << 20);

		let quic = crate::quic::Config {
			receive_window: Some(moq_net::transport::MIN_RECEIVE_WINDOW * 2),
			stream_receive_window: Some(256 * 1024),
			..Default::default()
		};
		let admission = moq_net::server::Admission::default();
		let mut options = Options::new(
			"ws://localhost/".parse().unwrap(),
			moq_net::Server::new().with_versions(moq_net::Versions::all()),
			admission.enter().unwrap(),
		);
		options.link.alpn = Some(alpn.clone());
		options.quic = quic.resolve();
		let server = tokio::spawn(accept(server_ws, options));

		let (session, driver) = qmux::ws::Upgraded::new(client_ws, &alpn).connect().await.unwrap();
		tokio::spawn(driver);
		// Runs until aborted: its driver sends SETUP, and ending it would cancel the request.
		let client = tokio::spawn(async move {
			let client = moq_net::Client::new().with_versions(moq_net::Versions::all());
			let (_session, driver) = client.connect(moq_net::time::Instant::now(), session).await?;
			moq_net::time::run(driver).await;
			moq_net::Result::Ok(())
		});

		let request = kio::clock::timeout(TIMEOUT, server)
			.await
			.expect("accept timeout")
			.expect("accept task")
			.expect("accept");
		let window = request.qmux_window();
		assert_eq!(
			window.connection,
			std::num::NonZeroU64::new(moq_net::transport::MIN_RECEIVE_WINDOW * 2)
		);
		assert_eq!(window.stream, 256 * 1024);
		client.abort();
	}

	/// Answer an upgrade offering only `qmux-01.moqt-22` with `response`, followed by
	/// `trailer`, and return what [`upgrade`] made of it plus the bytes left on the stream.
	async fn check_upgrade(response: impl Fn(&str) -> String, trailer: &[u8]) -> (Result<String>, Vec<u8>) {
		let (result, rest, _) = check_upgrade_request(response, trailer).await;
		(result, rest)
	}

	/// [`check_upgrade`], also returning the request head [`upgrade`] wrote.
	async fn check_upgrade_request(
		response: impl Fn(&str) -> String,
		trailer: &[u8],
	) -> (Result<String>, Vec<u8>, String) {
		let mut request = "ws://relay.example/room?jwt=token".into_client_request().unwrap();
		request.headers_mut().insert(
			http::header::SEC_WEBSOCKET_PROTOCOL,
			http::HeaderValue::from_static("qmux-01.moqt-22"),
		);
		let key = request.headers()[http::header::SEC_WEBSOCKET_KEY].clone();
		let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());

		let (mut client, mut server) = tokio::io::duplex(1 << 16);
		server.write_all(response(&accept).as_bytes()).await.unwrap();
		server.write_all(trailer).await.unwrap();
		let result = upgrade(&mut client, &request).await;
		let mut rest = vec![0; trailer.len()];
		client.read_exact(&mut rest).await.unwrap();
		drop(client);
		let mut head = String::new();
		server.read_to_string(&mut head).await.unwrap();
		(result, rest, head)
	}

	fn switching(accept: &str, protocol: &str) -> String {
		format!(
			"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: {protocol}\r\n\r\n"
		)
	}

	#[tokio::test]
	async fn upgrade_leaves_frames_behind_the_101() {
		let (result, rest) = check_upgrade(|accept| switching(accept, "qmux-01.moqt-22"), b"frames").await;
		assert_eq!(result.unwrap(), "qmux-01.moqt-22");
		assert_eq!(rest, b"frames");
	}

	#[tokio::test]
	async fn upgrade_requests_the_path_and_query() {
		let (result, _, head) = check_upgrade_request(|accept| switching(accept, "qmux-01.moqt-22"), b"").await;
		result.unwrap();
		assert!(head.starts_with("GET /room?jwt=token HTTP/1.1\r\n"), "{head}");
	}

	#[tokio::test]
	async fn upgrade_finds_the_token_in_a_later_connection_field() {
		let (result, _) = check_upgrade(
			|accept| {
				switching(accept, "qmux-01.moqt-22")
					.replace("Connection: Upgrade", "Connection: keep-alive\r\nConnection: Upgrade")
			},
			b"",
		)
		.await;
		assert_eq!(result.unwrap(), "qmux-01.moqt-22");
	}

	#[tokio::test]
	async fn upgrade_refuses_an_unoffered_subprotocol() {
		let (result, _) = check_upgrade(|accept| switching(accept, "qmux-00.moqt-22"), b"").await;
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn upgrade_refuses_a_missing_subprotocol() {
		let (result, _) = check_upgrade(
			|accept| {
				format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n")
			},
			b"",
		)
		.await;
		assert!(result.is_err());
	}

	#[tokio::test]
	async fn upgrade_refuses_a_101_that_is_not_a_websocket() {
		let (result, _) = check_upgrade(
			|accept| switching(accept, "qmux-01.moqt-22").replace("Upgrade: websocket", "Upgrade: h2c"),
			b"",
		)
		.await;
		assert!(result.is_err());
	}

	#[test]
	fn moqt_18_and_newer_pin_to_qmux01() {
		// The literals in `qmux_versions_for` must stay the IETF draft ALPNs;
		// otherwise the pin silently stops matching.
		let drafts: Vec<_> = ["moq-transport-18", "moq-transport-20", "moq-transport-22"]
			.into_iter()
			.map(|name| name.parse::<moq_net::Version>().unwrap().alpn())
			.collect();
		assert_eq!(QMUX01_ONLY_ALPNS, drafts);
		for &alpn in QMUX01_ONLY_ALPNS {
			assert_eq!(qmux_versions_for(alpn), &[qmux::Version::QMux01]);
		}

		// Everything else stays unrestricted (qmux expands `&[]` to all drafts).
		for &alpn in moq_net::ALPNS {
			if !QMUX01_ONLY_ALPNS.contains(&alpn) {
				assert!(qmux_versions_for(alpn).is_empty(), "{alpn} should not be pinned");
			}
		}
	}
}

#[cfg(test)]
mod legacy_tests {
	use super::*;
	#[derive(usage_rs::Cli)]
	#[usage(unknown_flags = "error", args_override_self = false)]
	#[usage(settings)]
	struct Cli {
		#[usage(flatten)]
		websocket: Config,
	}

	fn parse(args: &[&str]) -> Config {
		let argv = args.iter().map(std::ffi::OsStr::new).collect::<Vec<_>>();
		Cli::parse_from(&argv).unwrap().websocket
	}

	/// The released env vars have hidden flags of their own, so they are recognized
	/// and reported rather than silently ignored.
	#[test]
	fn released_spellings_are_reported_not_applied() {
		let config = parse(&["--websocket-enabled=false", "--websocket-delay", "1s"]);
		let resolved = config.resolve();
		assert!(resolved.enabled, "the released spelling must not turn it off");
		assert_eq!(resolved.delay, DEFAULT_DELAY);

		let reported = config.deprecated().to_string();
		assert!(
			reported.contains("--websocket-enabled / MOQ_CLIENT_WEBSOCKET_ENABLED -> --connect-websocket-enabled"),
			"{reported}"
		);
		assert!(reported.contains("--websocket-delay"), "{reported}");
	}

	#[test]
	fn canonical_spellings_apply() {
		let config = parse(&["--connect-websocket-enabled=false", "--connect-websocket-delay", "2s"]);
		assert!(config.deprecated().is_empty());
		let resolved = config.resolve();
		assert!(!resolved.enabled);
		assert_eq!(resolved.delay, time::Duration::from_secs(2));

		// Neither given: the parser resolves to the typed defaults.
		let config = parse(&[]);
		assert_eq!(config.resolve().delay, DEFAULT_DELAY);
	}
}

#[cfg(test)]
mod duration_tests {
	use super::*;
	crate::cli::duration::test_config_duration!(
		delay_set_in_code_beats_parser_default,
		Config,
		delay,
		"--connect-websocket-delay",
		|c: &Config| c.resolve().delay,
		|d| d
	);
}

#[cfg(test)]
mod timeout_tests {
	use super::*;
	use futures::FutureExt;
	use moq_net::transport::poll::Session as _;

	/// A peer that upgrades but never sends its QMux parameters is refused at the accept's own
	/// deadline, not QMux's longer handshake timeout.
	#[tokio::test(start_paused = true)]
	async fn silent_qmux_peer_is_refused_at_the_deadline() {
		const ALPN: &str = "qmux-01.moq-lite-06";
		let admission = moq_net::server::Admission::default();
		let timeout = time::Duration::from_secs(1);
		let mut options = Options::new(
			"ws://localhost/".parse().unwrap(),
			moq_net::Server::new().with_timeout(Some(timeout)),
			admission.enter().unwrap(),
		);
		options.link.alpn = Some(ALPN.into());
		let (server, _peer) = tokio::io::duplex(4096);
		let start = kio::clock::now();
		let result = accept(server, options).await;
		assert!(
			matches!(result, Err(crate::Error::MoqNet(moq_net::Error::Timeout))),
			"accept outcome: {:?}",
			result.map(|_| ())
		);
		assert_eq!(start.elapsed(), timeout);
	}

	#[tokio::test(start_paused = true)]
	async fn the_servers_timeout_bounds_setup() {
		const ALPN: &str = "qmux-01.moq-lite-06";
		let short = time::Duration::from_secs(1);
		for (timeout, expires) in [(Some(short), true), (None, false)] {
			let admission = moq_net::server::Admission::default();
			let mut options = Options::new(
				"ws://localhost/".parse().unwrap(),
				moq_net::Server::new().with_timeout(timeout),
				admission.enter().unwrap(),
			);
			options.link.alpn = Some(ALPN.into());
			// Complete QMux over an in-memory WebSocket, then withhold MoQ SETUP.
			// A silent transport would expire QMux's own parameter deadline first.
			let (server, peer) = tokio::io::duplex(4096);
			let mut accept = Box::pin(accept(server, options));
			// Each end waits for the other's QMux parameters, so the accept runs meanwhile.
			let peer = tokio::select! {
				peer = qmux::ws::Upgraded::new(peer, ALPN).connect() => crate::spawn_qmux(peer.unwrap()),
				result = accept.as_mut() => panic!("accepted before SETUP: {:?}", result.map(|_| ())),
			};
			let _stream = kio::wait(|waiter| peer.poll_open_bi(waiter, Default::default()))
				.await
				.unwrap();
			tokio::time::advance(timeout.unwrap_or(crate::listen::DEFAULT_TIMEOUT)).await;
			let result = accept.as_mut().now_or_never();
			if expires {
				assert!(
					matches!(result, Some(Err(crate::Error::MoqNet(moq_net::Error::Timeout)))),
					"accept outcome: {:?}",
					result.map(|outcome| outcome.map(|_| ()))
				);
			} else {
				assert!(result.is_none(), "no timeout must keep waiting for SETUP");
			}
		}
	}
}
