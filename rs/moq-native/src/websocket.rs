use anyhow::Context;
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::time;
use url::Url;
use web_transport_ws::{tokio_tungstenite, tungstenite};

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
) -> Option<anyhow::Result<web_transport_ws::Session>> {
	if !config.enabled {
		return None;
	}
	let res = connect(config, tls, url).await;
	if let Err(err) = &res {
		tracing::warn!(%err, "WebSocket connection failed");
	}
	Some(res)
}

pub(crate) async fn connect(
	config: &ClientWebSocket,
	tls: &rustls::ClientConfig,
	mut url: Url,
) -> anyhow::Result<web_transport_ws::Session> {
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
	let needs_tls = match url.scheme() {
		"http" => {
			url.set_scheme("ws").expect("failed to set scheme");
			false
		}
		"https" | "moql" | "moqt" => {
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
		Some(tokio_tungstenite::Connector::Rustls(Arc::new(tls.clone())))
	} else {
		None
	};

	// Connect using tokio-tungstenite
	let (ws_stream, _response) = tokio_tungstenite::connect_async_tls_with_config(
		url.as_str(),
		Some(tungstenite::protocol::WebSocketConfig {
			max_message_size: Some(64 << 20), // 64 MB
			max_frame_size: Some(16 << 20),   // 16 MB
			accept_unmasked_frames: false,
			..Default::default()
		}),
		false, // disable_nagle
		connector,
	)
	.await
	.context("failed to connect WebSocket")?;

	// Wrap WebSocket in WebTransport compatibility layer
	// Similar to what the relay does: web_transport_ws::Session::new(socket, true)
	let session = web_transport_ws::Session::new(ws_stream, false);

	tracing::warn!(%url, "using WebSocket fallback");
	WEBSOCKET_WON.lock().unwrap().insert(key);

	Ok(session)
}
