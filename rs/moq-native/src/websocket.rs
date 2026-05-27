use anyhow::Context;
use moq_net::QmuxVersion;
use qmux::tokio_tungstenite;
use qmux::tungstenite;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::{net, time};
use url::Url;

// Track servers (hostname:port) where WebSocket won the race, so we won't give QUIC a headstart next time
static WEBSOCKET_WON: LazyLock<Mutex<HashSet<(String, u16)>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// WebSocket configuration for the client.
#[derive(Clone, Debug, clap::Args, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct ClientWebSocket {
	/// Whether to enable WebSocket support.
	#[arg(
		id = "websocket-enabled",
		long = "websocket-enabled",
		env = "MOQ_CLIENT_WEBSOCKET_ENABLED",
		default_value = "true"
	)]
	pub enabled: bool,

	/// Delay in milliseconds before attempting WebSocket fallback (default: 200)
	/// If WebSocket won the previous race for a given server, this will be 0.
	#[arg(
		id = "websocket-delay",
		long = "websocket-delay",
		env = "MOQ_CLIENT_WEBSOCKET_DELAY",
		default_value = "200ms",
		value_parser = humantime::parse_duration,
	)]
	#[serde(with = "humantime_serde")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub delay: Option<time::Duration>,
}

impl Default for ClientWebSocket {
	fn default() -> Self {
		Self {
			enabled: true,
			delay: Some(time::Duration::from_millis(200)),
		}
	}
}

fn qmux_version(qv: QmuxVersion) -> qmux::Version {
	// `QmuxVersion` is `#[non_exhaustive]`, so we need a fallthrough even
	// though both current variants are listed. New variants will hit this
	// arm at runtime, which we'd want to update to handle cleanly.
	match qv {
		QmuxVersion::QMux00 => qmux::Version::QMux00,
		QmuxVersion::QMux01 => qmux::Version::QMux01,
		_ => unreachable!("unknown QmuxVersion variant"),
	}
}

/// Format a `(QmuxVersion, app)` pair as the `qmux-XX.app` subprotocol string.
fn pair_to_alpn(qv: QmuxVersion, app: &str) -> String {
	format!("{}.{}", qv.alpn(), app)
}

pub(crate) async fn race_handle(
	config: &ClientWebSocket,
	tls: &rustls::ClientConfig,
	url: Url,
	alpns: &[(QmuxVersion, &str)],
) -> Option<anyhow::Result<qmux::Session>> {
	if !config.enabled {
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
	config: &ClientWebSocket,
	tls: &rustls::ClientConfig,
	mut url: Url,
	alpns: &[(QmuxVersion, &str)],
) -> anyhow::Result<qmux::Session> {
	anyhow::ensure!(config.enabled, "WebSocket support is disabled");
	anyhow::ensure!(!alpns.is_empty(), "no WebSocket subprotocols to offer");

	let host = url.host_str().context("missing hostname")?.to_string();
	let port = url.port().unwrap_or_else(|| match url.scheme() {
		"https" | "wss" | "moql" | "moqt" => 443,
		"http" | "ws" => 80,
		_ => 443,
	});
	let key = (host, port);

	// Apply a small penalty to WebSocket to improve odds for QUIC to connect first,
	// unless we've already had to fall back to WebSockets for this server.
	// TODO if let chain
	match config.delay {
		Some(delay) if !WEBSOCKET_WON.lock().unwrap().contains(&key) => {
			tokio::time::sleep(delay).await;
			tracing::debug!(%url, delay_ms = %delay.as_millis(), "QUIC not yet connected, attempting WebSocket fallback");
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
		_ => anyhow::bail!("unsupported URL scheme for WebSocket: {}", url.scheme()),
	};

	tracing::debug!(%url, "connecting via WebSocket");

	// Use the existing TLS config (which respects tls-disable-verify) for secure connections
	let connector = if needs_tls {
		tokio_tungstenite::Connector::Rustls(Arc::new(tls.clone()))
	} else {
		tokio_tungstenite::Connector::Plain
	};

	// Build the request ourselves so we can advertise the full `qmux-XX.app`
	// pair list in a single connection. qmux is one-version-per-connection;
	// moq-native owns the multi-version multiplexing.
	use tungstenite::client::IntoClientRequest;
	let mut request = url.as_str().into_client_request().context("invalid WebSocket URL")?;
	let formatted: Vec<String> = alpns.iter().map(|(qv, app)| pair_to_alpn(*qv, app)).collect();
	let protocol_value = formatted.join(", ");
	request.headers_mut().insert(
		tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL,
		tungstenite::http::HeaderValue::from_str(&protocol_value).context("invalid Sec-WebSocket-Protocol value")?,
	);

	let (ws, response) = tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
		.await
		.context("failed to connect WebSocket")?;

	let negotiated = response
		.headers()
		.get(tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL)
		.and_then(|h| h.to_str().ok())
		.context("server did not select a Sec-WebSocket-Protocol")?;
	// The server can only pick something we offered, so we recover the qmux
	// version by index in the pair list rather than re-parsing the prefix.
	let idx = formatted
		.iter()
		.position(|s| s == negotiated)
		.with_context(|| format!("server picked an alpn we did not offer: {negotiated}"))?;
	let (qv, _) = alpns[idx];

	let session = qmux::ws::Upgraded::new(ws, qmux_version(qv))
		.with_alpn(negotiated)
		.connect();

	tracing::warn!(%url, ?qv, %negotiated, "using WebSocket fallback");
	WEBSOCKET_WON.lock().unwrap().insert(key);

	Ok(session)
}

/// Listens for incoming WebSocket connections on a TCP port.
///
/// Use with [`crate::Server::with_websocket`] to accept WebSocket connections
/// alongside QUIC connections on a separate port.
pub struct WebSocketListener {
	listener: tokio::net::TcpListener,
	pairs: &'static [(QmuxVersion, &'static str)],
	// Pre-formatted `qmux-XX.app` strings, same order as `pairs`. The handshake
	// callback matches against these and we look up the qmux version by index.
	formatted: Arc<Vec<String>>,
}

impl WebSocketListener {
	pub async fn bind(addr: net::SocketAddr) -> anyhow::Result<Self> {
		Self::bind_with_alpns(addr, moq_net::QMUX_ALPNS).await
	}

	pub async fn bind_with_alpns(
		addr: net::SocketAddr,
		alpns: &'static [(QmuxVersion, &'static str)],
	) -> anyhow::Result<Self> {
		anyhow::ensure!(!alpns.is_empty(), "no WebSocket subprotocols to accept");
		let listener = tokio::net::TcpListener::bind(addr).await?;
		let formatted = alpns.iter().map(|(qv, app)| pair_to_alpn(*qv, app)).collect();
		Ok(Self {
			listener,
			pairs: alpns,
			formatted: Arc::new(formatted),
		})
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	pub async fn accept(&self) -> Option<anyhow::Result<qmux::Session>> {
		match self.listener.accept().await {
			Ok((stream, addr)) => {
				tracing::debug!(%addr, "accepted WebSocket TCP connection");
				Some(accept_socket(stream, self.pairs, self.formatted.clone()).await)
			}
			Err(e) => Some(Err(e.into())),
		}
	}
}

async fn accept_socket(
	stream: tokio::net::TcpStream,
	pairs: &'static [(QmuxVersion, &'static str)],
	formatted: Arc<Vec<String>>,
) -> anyhow::Result<qmux::Session> {
	use std::sync::Mutex;
	use tungstenite::handshake::server;
	use tungstenite::http;

	// Capture the negotiated string from inside the handshake callback.
	let chosen_slot: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
	let slot = chosen_slot.clone();
	let supported = formatted.clone();

	#[allow(clippy::result_large_err)]
	let callback = move |req: &server::Request,
	                  mut response: server::Response|
	      -> Result<server::Response, server::ErrorResponse> {
		let header_protocols: Vec<&str> = req
			.headers()
			.get_all(http::header::SEC_WEBSOCKET_PROTOCOL)
			.iter()
			.filter_map(|v| v.to_str().ok())
			.flat_map(|h| h.split(','))
			.map(|p| p.trim())
			.filter(|p| !p.is_empty())
			.collect();

		// Pick the first server-supported protocol that the client offered.
		match supported.iter().find(|s| header_protocols.contains(&s.as_str())) {
			Some(picked) => {
				response.headers_mut().insert(
					http::header::SEC_WEBSOCKET_PROTOCOL,
					http::HeaderValue::from_str(picked).expect("alpn must be valid HTTP value"),
				);
				*slot.lock().unwrap() = Some(picked.clone());
				Ok(response)
			}
			None => Err(http::Response::builder()
				.status(http::StatusCode::BAD_REQUEST)
				.body(Some("no supported Sec-WebSocket-Protocol".to_string()))
				.unwrap()),
		}
	};

	let ws = tokio_tungstenite::accept_hdr_async_with_config(stream, callback, None)
		.await
		.context("WebSocket handshake failed")?;

	let negotiated = chosen_slot
		.lock()
		.unwrap()
		.take()
		.context("handshake completed without setting negotiated protocol")?;
	let idx = formatted
		.iter()
		.position(|s| *s == negotiated)
		.expect("callback only writes strings drawn from `formatted`");
	let (qv, _) = pairs[idx];

	Ok(qmux::ws::Upgraded::new(ws, qmux_version(qv))
		.with_alpn(&negotiated)
		.accept())
}
