use anyhow::Context;
use qmux::tokio_tungstenite;
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

pub(crate) async fn race_handle(
	config: &ClientWebSocket,
	tls: &rustls::ClientConfig,
	url: Url,
	alpns: &[&str],
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
	alpns: &[&str],
) -> anyhow::Result<qmux::Session> {
	anyhow::ensure!(config.enabled, "WebSocket support is disabled");

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

	let session = qmux::Client::new()
		.with_protocols(alpns)
		.with_connector(connector)
		.connect(url.as_str())
		.await
		.context("failed to connect WebSocket")?;

	tracing::warn!(%url, "using WebSocket fallback");
	WEBSOCKET_WON.lock().unwrap().insert(key);

	Ok(session)
}

/// Listens for incoming WebSocket connections on a TCP port.
///
/// Use with [`crate::Server::with_websocket`] to accept WebSocket connections
/// alongside QUIC connections on a separate port.
pub struct WebSocketListener {
	listener: tokio::net::TcpListener,
	server: qmux::Server,
}

impl WebSocketListener {
	pub async fn bind(addr: net::SocketAddr) -> anyhow::Result<Self> {
		Self::bind_with_alpns(addr, moq_lite::ALPNS).await
	}

	pub async fn bind_with_alpns(addr: net::SocketAddr, alpns: &[&str]) -> anyhow::Result<Self> {
		let listener = tokio::net::TcpListener::bind(addr).await?;
		let server = qmux::Server::new().with_protocols(alpns);
		Ok(Self { listener, server })
	}

	pub fn local_addr(&self) -> anyhow::Result<net::SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	pub async fn accept(&self) -> Option<anyhow::Result<qmux::Session>> {
		match self.listener.accept().await {
			Ok((stream, addr)) => {
				tracing::debug!(%addr, "accepted WebSocket TCP connection");
				let server = self.server.clone();
				Some(
					server
						.accept(stream)
						.await
						.map_err(|e| anyhow::anyhow!("WebSocket accept failed: {e}")),
				)
			}
			Err(e) => Some(Err(e.into())),
		}
	}
}
