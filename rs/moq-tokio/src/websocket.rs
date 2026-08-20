//! WebSocket fallback transport, running the QMux wire format over `ws://` or `wss://`.
//!
//! Used when QUIC is unreachable: UDP blocked by a firewall, a proxy in the way, a
//! network that only passes TCP/443. The client races this against QUIC and gives QUIC
//! a small head start ([`Config::delay`]), so WebSocket only wins when QUIC can't get
//! through. Servers accept it on a separate TCP port via [`Listener`].

use qmux::ws::tokio_tungstenite;
use qmux::ws::tokio_tungstenite::tungstenite::{self, http};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::{net, time};
use url::Url;

/// Errors specific to the WebSocket fallback backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
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

	/// The URL scheme can't carry WebSocket. Only `http`, `https`, `ws`, and `wss` work.
	#[error("unsupported URL scheme for WebSocket: {0}")]
	UnsupportedScheme(String),

	/// The qmux handshake failed while dialing, including a non-101 upgrade response
	/// from the server.
	#[error("failed to connect WebSocket")]
	Connect(#[source] qmux::Error),

	/// The URL couldn't be turned into a valid WebSocket handshake request.
	#[error("failed to build WebSocket request")]
	BuildRequest(#[source] tungstenite::Error),

	/// An ALPN contained bytes that aren't legal in the `Sec-WebSocket-Protocol` header.
	#[error("failed to build WebSocket protocols header")]
	ProtocolHeader(#[source] http::header::InvalidHeaderValue),

	/// The TCP/TLS connection or the WebSocket upgrade itself failed.
	#[error("failed to connect WebSocket")]
	WebSocketConnect(#[source] tungstenite::Error),

	/// The server refused the connection outright, so retrying won't help.
	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

	/// The qmux handshake failed while accepting an incoming connection.
	#[error("WebSocket accept failed")]
	Accept(#[source] qmux::Error),
}

type Result<T> = std::result::Result<T, Error>;

// Track servers (hostname:port) where WebSocket won the race, so we won't give QUIC a headstart next time
static WEBSOCKET_WON: LazyLock<Mutex<HashSet<(String, u16)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// WebSocket configuration for the client.
#[derive(Clone, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[group(id = "websocket-client")]
#[non_exhaustive]
#[derive(Default)]
pub struct Config {
	/// Whether to enable the WebSocket fallback. Defaults to true.
	///
	/// `Option` with the default resolved in code rather than a clap `default_value`,
	/// which a config file could not override: the CLI is re-parsed after the file is
	/// read, so a materialized default would win.
	#[arg(
		id = "connect-websocket-enabled",
		long = "connect-websocket-enabled",
		env = "MOQ_CONNECT_WEBSOCKET_ENABLED",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub enabled: Option<bool>,

	/// Head start given to the QUIC dial before the WebSocket fallback joins the
	/// race. Defaults to 200ms, and drops to zero for a server WebSocket already won.
	#[arg(
		id = "connect-websocket-delay",
		long = "connect-websocket-delay",
		env = "MOQ_CONNECT_WEBSOCKET_DELAY",
		value_parser = humantime::parse_duration,
	)]
	#[serde(default, with = "humantime_serde::option")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub delay: Option<time::Duration>,

	/// The released `MOQ_CLIENT_WEBSOCKET_*` env vars, folded in by [`Config::resolved`].
	#[command(flatten)]
	#[serde(skip)]
	pub(crate) legacy: Legacy,
}

/// The released spellings for this section, which moved to `--connect-websocket-*`
/// and `MOQ_CONNECT_WEBSOCKET_*` with the rest of the dial side.
///
/// Separate args rather than clap aliases, since an alias renames the flag but
/// leaves its env var behind.
#[derive(Clone, Debug, Default, clap::Args)]
#[group(id = "websocket-legacy")]
pub(crate) struct Legacy {
	#[arg(
		id = "websocket-enabled",
		long = "websocket-enabled",
		alias = "client-websocket-enabled",
		env = "MOQ_CLIENT_WEBSOCKET_ENABLED",
		hide = true,
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	enabled: Option<bool>,

	#[arg(
		id = "websocket-delay",
		long = "websocket-delay",
		alias = "client-websocket-delay",
		env = "MOQ_CLIENT_WEBSOCKET_DELAY",
		hide = true,
		value_parser = humantime::parse_duration,
	)]
	delay: Option<time::Duration>,
}

/// The QUIC head start when `--connect-websocket-delay` is unset.
const DEFAULT_DELAY: time::Duration = time::Duration::from_millis(200);

impl Config {
	/// The released spellings in use, each paired with what replaced it. Reached
	/// through [`crate::connect::Config::deprecated`].
	pub(crate) fn deprecated(&self) -> crate::Deprecated {
		let mut found = crate::Deprecated::default();
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

	/// Whether the fallback runs at all, resolving the default.
	pub fn resolved_enabled(&self) -> bool {
		self.enabled.unwrap_or(true)
	}

	/// The head start a QUIC dial gets, resolving the default.
	pub fn resolved_delay(&self) -> time::Duration {
		self.delay.unwrap_or(DEFAULT_DELAY)
	}
}

/// The fallback arm of the QUIC-vs-WebSocket race, so only compiled when there is a
/// QUIC dial to race against. A WebSocket-only build calls [`connect`] directly.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
pub(crate) async fn race_handle(
	config: &Config,
	tls: &rustls::ClientConfig,
	url: Url,
	alpns: &[&str],
) -> Option<Result<qmux::Session>> {
	if !config.resolved_enabled() {
		return None;
	}

	// Only attempt WebSocket for HTTP-based schemes.
	// Custom protocols (moqt://, moql://) use raw QUIC and don't support WebSocket.
	match url.scheme() {
		"http" | "https" | "ws" | "wss" => {}
		_ => return None,
	}

	let res = connect(config, tls, url, alpns).await;
	if let Err(err) = &res {
		tracing::warn!(%err, "WebSocket connection failed");
	}
	Some(res)
}

pub(crate) async fn connect(
	config: &Config,
	tls: &rustls::ClientConfig,
	mut url: Url,
	alpns: &[&str],
) -> Result<qmux::Session> {
	if !config.resolved_enabled() {
		return Err(Error::Disabled);
	}

	let host = url.host_str().ok_or(Error::MissingHostname)?.to_string();
	let port = url.port().unwrap_or_else(|| match url.scheme() {
		"https" | "wss" | "moql" | "moqt" => 443,
		"http" | "ws" => 80,
		_ => 443,
	});
	let key = (host, port);

	// Apply a small penalty to WebSocket to improve odds for QUIC to connect first,
	// unless we've already had to fall back to WebSockets for this server.
	// TODO if let chain
	match config.resolved_delay() {
		delay if !delay.is_zero() && !WEBSOCKET_WON.lock().unwrap().contains(&key) => {
			tokio::time::sleep(delay).await;
			tracing::debug!(peer = %crate::connect::Endpoint(&url), delay_ms = %delay.as_millis(), "QUIC not yet connected, attempting WebSocket fallback");
		}
		_ => {}
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

	tracing::debug!(peer = %crate::connect::Endpoint(&url), "connecting via WebSocket");

	// Use the existing TLS config (which respects tls-disable-verify) for secure connections.
	let connector = if needs_tls {
		tokio_tungstenite::Connector::Rustls(Arc::new(tls.clone()))
	} else {
		tokio_tungstenite::Connector::Plain
	};

	// Most moq ALPNs can ride on any QMux draft (`&[]` lets the polyfill expand
	// to every version it knows). `qmux_versions_for` pins the few that the spec
	// restricts. qmux also offers the bare ALPNs (`qmux-01`, `qmux-00`,
	// `webtransport`) by default so we still interop with relays that only know a
	// wire-format version.
	let session = qmux::ws::Client::new()
		.with_protocols(alpns.iter().map(|&a| (a, qmux_versions_for(a))))
		.with_connector(connector)
		.with_keep_alive(qmux::ws::KeepAlive::default()) // 5s ping / 30s deadline, parity with QUIC
		.connect(url.as_str())
		.await
		.map_err(Error::Connect)?;

	tracing::warn!(peer = %crate::connect::Endpoint(&url), "using WebSocket fallback");
	WEBSOCKET_WON.lock().unwrap().insert(key);

	Ok(session)
}

/// The QMux drafts a moq ALPN is allowed to ride on, for `qmux::ws::Client::with_protocols`.
///
/// moq-transport-18 and -19 require qmux-01, so we never pair them with qmux-00.
/// This mirrors the policy in `js/net`'s `connect.ts`. Every other ALPN returns
/// `&[]`, which qmux expands to every draft it knows about.
const QMUX01_ONLY_ALPNS: &[&str] = &["moqt-18", "moqt-19"];

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
			// qmux surfaces a non-101 WebSocket upgrade response as `Http(status)`;
			// map an auth rejection (401/403) so the caller sees it as terminal.
			Self::Connect(qmux::Error::Http(status)) => crate::ConnectError::from_status_u16(*status),
			_ => None,
		}
	}

	/// The HTTP status the server answered the upgrade with, if it answered with one at all.
	///
	/// qmux surfaces a non-101 WebSocket upgrade response as `Http(status)`. See
	/// [`crate::Error::status`].
	pub(crate) fn status(&self) -> Option<u16> {
		match self {
			Self::Connect(qmux::Error::Http(status)) => Some(*status),
			_ => None,
		}
	}
}

/// Listens for incoming WebSocket connections on a TCP port.
///
/// Use with [`crate::Server::with_websocket`] to accept WebSocket connections
/// alongside QUIC connections on a separate port.
pub struct Listener {
	listener: tokio::net::TcpListener,
	protocols: Vec<String>,
	health: crate::accept::Health,
}

impl Listener {
	/// Bind a listener to the given address, accepting every moq ALPN we know about.
	pub async fn bind(addr: net::SocketAddr) -> Result<Self> {
		Self::bind_with_alpns(addr, moq_net::ALPNS).await
	}

	/// Bind a listener that only accepts the given moq ALPNs, in preference order.
	pub async fn bind_with_alpns(addr: net::SocketAddr, alpns: &[&str]) -> Result<Self> {
		let listener = tokio::net::TcpListener::bind(addr).await?;
		let protocols = supported_subprotocols(alpns);
		for protocol in &protocols {
			http::HeaderValue::from_str(protocol).map_err(Error::ProtocolHeader)?;
		}
		Ok(Self {
			listener,
			protocols,
			health: crate::accept::Health::new("websocket"),
		})
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
	/// per-connection upgrade failure is still yielded as `Some(Err(..))`.
	///
	/// As in [`crate::tcp`], the `Option` has no `None` case left to report.
	pub async fn accept(&self) -> Option<Result<qmux::Session>> {
		self.accept_with_url()
			.await
			.map(|result| result.map(|(session, _)| session))
	}

	/// Accept the next connection and retain the WebSocket request URL.
	pub(crate) async fn accept_with_url(&self) -> Option<Result<(qmux::Session, Url)>> {
		let (stream, addr) = self.accept_socket().await;
		tracing::debug!(%addr, "accepted WebSocket TCP connection");

		let accepted = Arc::new(Mutex::new(None::<(Option<String>, Url)>));
		let accepted_callback = accepted.clone();
		let protocols = self.protocols.clone();
		#[allow(clippy::result_large_err)]
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
			let Ok(protocol) = select_subprotocol(&offered, &protocols) else {
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

			if let Some(protocol) = protocol {
				response.headers_mut().insert(
					http::header::SEC_WEBSOCKET_PROTOCOL,
					http::HeaderValue::from_str(protocol).expect("protocol validated at bind"),
				);
			}
			*accepted_callback.lock().unwrap() = Some((protocol.map(str::to_string), url));
			Ok(response)
		};

		let websocket = tokio_tungstenite::accept_hdr_async_with_config(stream, callback, None)
			.await
			.map_err(qmux::Error::from)
			.map_err(Error::Accept);
		Some(websocket.map(|websocket| {
			let (protocol, url) = accepted
				.lock()
				.unwrap()
				.take()
				.expect("successful upgrade selected a protocol");
			let upgraded = qmux::ws::Upgraded::new(websocket).with_keep_alive(qmux::ws::KeepAlive::default());
			let session = match protocol {
				Some(protocol) => upgraded.with_alpn(&protocol).accept(),
				None => upgraded.accept(),
			};
			(session, url)
		}))
	}

	/// The `accept(2)` half: keep asking until a connection comes back.
	async fn accept_socket(&self) -> (tokio::net::TcpStream, net::SocketAddr) {
		loop {
			match self.listener.accept().await {
				Ok(accepted) => {
					self.health.accepted();
					return accepted;
				}
				Err(err) => {
					if let Some(delay) = self.health.failed(&err) {
						tokio::time::sleep(delay).await;
					}
				}
			}
		}
	}
}

/// Select a supported subprotocol, while preserving legacy clients that offer none.
fn select_subprotocol<'a>(offered: &[&str], supported: &'a [String]) -> std::result::Result<Option<&'a str>, ()> {
	if offered.is_empty() {
		return Ok(None);
	}

	supported
		.iter()
		.find(|protocol| offered.contains(&protocol.as_str()))
		.map(|protocol| Some(protocol.as_str()))
		.ok_or(())
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
				.filter(|version| version.is_qmux())
				.map(|version| format!("{}{alpn}", version.prefix())),
		);
	}
	protocols.extend(qmux::ALPNS.iter().map(|protocol| (*protocol).to_string()));
	protocols
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn subprotocol_selection_preserves_legacy_clients() {
		let supported = vec!["qmux-01.moq-lite-05".to_string()];
		assert_eq!(select_subprotocol(&[], &supported), Ok(None));
		assert_eq!(
			select_subprotocol(&["qmux-01.moq-lite-05"], &supported),
			Ok(Some("qmux-01.moq-lite-05"))
		);
		assert_eq!(select_subprotocol(&["unsupported"], &supported), Err(()));
	}

	#[tokio::test]
	async fn listener_accepts_legacy_client_without_subprotocol() {
		let listener = Listener::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
		let addr = listener.local_addr().unwrap();
		let accepted = tokio::spawn(async move { listener.accept_with_url().await.unwrap().unwrap() });

		let stream = tokio::net::TcpStream::connect(addr).await.unwrap();
		let request_url = format!("ws://{addr}/room?jwt=test");
		let (websocket, response) = tokio_tungstenite::client_async(request_url, stream).await.unwrap();
		assert!(!response.headers().contains_key(http::header::SEC_WEBSOCKET_PROTOCOL));

		let (session, url) = accepted.await.unwrap();
		assert_eq!(url.path(), "/room");
		assert_eq!(url.query(), Some("jwt=test"));
		drop(session);
		drop(websocket);
	}

	#[test]
	fn moqt_18_and_19_pin_to_qmux01() {
		// The literals in `qmux_versions_for` must stay the IETF draft ALPNs;
		// otherwise the pin silently stops matching.
		assert_eq!(
			QMUX01_ONLY_ALPNS
				.iter()
				.map(|&a| moq_net::Version::from_alpn(a).map(|v| v.code()))
				.collect::<Vec<_>>(),
			vec![Some(0xff000012), Some(0xff000013)]
		);
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
	use clap::Parser;

	#[derive(Parser)]
	struct Cli {
		#[command(flatten)]
		websocket: Config,
	}

	fn parse(args: &[&str]) -> Config {
		let mut argv = vec!["test"];
		argv.extend_from_slice(args);
		Cli::parse_from(argv).websocket
	}

	/// The released env vars have hidden flags of their own, so they are recognized
	/// and reported rather than silently ignored.
	#[test]
	fn released_spellings_are_reported_not_applied() {
		let config = parse(&["--websocket-enabled=false", "--websocket-delay", "1s"]);
		assert!(config.resolved_enabled(), "the released spelling must not turn it off");
		assert_eq!(config.resolved_delay(), DEFAULT_DELAY);

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
		assert!(!config.resolved_enabled());
		assert_eq!(config.resolved_delay(), time::Duration::from_secs(2));

		// Neither given: the defaults, which no config file can be clobbered out of.
		let config = parse(&[]);
		assert_eq!(config.delay, None);
		assert_eq!(config.resolved_delay(), DEFAULT_DELAY);
	}
}
