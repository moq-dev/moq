use qmux::tokio_tungstenite;
use qmux::tokio_tungstenite::tungstenite::{self, client::IntoClientRequest, http};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::{net, time};
use url::Url;

/// Errors specific to the WebSocket fallback backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error("WebSocket support is disabled")]
	Disabled,

	#[error("missing hostname")]
	MissingHostname,

	#[error("unsupported URL scheme for WebSocket: {0}")]
	UnsupportedScheme(String),

	#[error("failed to connect WebSocket")]
	Connect(#[source] qmux::Error),

	#[error("failed to build WebSocket request")]
	BuildRequest(#[source] tungstenite::Error),

	#[error("failed to build WebSocket protocols header")]
	ProtocolHeader(#[source] http::header::InvalidHeaderValue),

	#[error("failed to connect WebSocket")]
	WebSocketConnect(#[source] tungstenite::Error),

	#[error(transparent)]
	ConnectRejected(#[from] crate::ConnectError),

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
pub struct Client {
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

impl Default for Client {
	fn default() -> Self {
		Self {
			enabled: true,
			delay: Some(time::Duration::from_millis(200)),
		}
	}
}

pub(crate) async fn race_handle(
	config: &Client,
	tls: &rustls::ClientConfig,
	url: Url,
	alpns: &[&str],
) -> Option<Result<qmux::Session>> {
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
	config: &Client,
	tls: &rustls::ClientConfig,
	mut url: Url,
	alpns: &[&str],
) -> Result<qmux::Session> {
	if !config.enabled {
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
		_ => return Err(Error::UnsupportedScheme(url.scheme().to_string())),
	};

	tracing::debug!(%url, "connecting via WebSocket");

	// Use the existing TLS config (which respects tls-disable-verify) for secure connections.
	let connector = if needs_tls {
		tokio_tungstenite::Connector::Rustls(Arc::new(tls.clone()))
	} else {
		tokio_tungstenite::Connector::Plain
	};

	let mut request = url.as_str().into_client_request().map_err(Error::BuildRequest)?;
	let protocols = websocket_subprotocols(alpns).join(", ");
	request.headers_mut().insert(
		http::header::SEC_WEBSOCKET_PROTOCOL,
		http::HeaderValue::from_str(&protocols).map_err(Error::ProtocolHeader)?,
	);

	let (socket, response) = if needs_tls {
		tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector))
			.await
			.map_err(map_websocket_error)?
	} else {
		tokio_tungstenite::connect_async_with_config(request, None, false)
			.await
			.map_err(map_websocket_error)?
	};

	let alpn = response
		.headers()
		.get(http::header::SEC_WEBSOCKET_PROTOCOL)
		.and_then(|header| header.to_str().ok())
		.map(str::to_owned);
	let upgraded = qmux::ws::Upgraded::new(socket).with_keep_alive(qmux::KeepAlive::default());
	let upgraded = match alpn.as_deref() {
		Some(alpn) => upgraded.with_alpn(alpn),
		None => upgraded,
	};
	let session = upgraded.connect();

	tracing::warn!(%url, "using WebSocket fallback");
	WEBSOCKET_WON.lock().unwrap().insert(key);

	Ok(session)
}

fn websocket_subprotocols(alpns: &[&str]) -> Vec<String> {
	// Each moq ALPN under every QMux wire version (`qmux-01.moq-lite-04`, ...),
	// newest first, then the bare qmux fallbacks. Mirrors qmux's own ALPN
	// builder, which isn't public.
	//
	// `qmux-00.moqt-18` is excluded: moq-transport-18 requires qmux-01, so that
	// pair is illegal (matches the relay and js/net's connect.ts).
	let versions = [qmux::Version::QMux01, qmux::Version::QMux00];
	let mut protocols = Vec::with_capacity(versions.len() * alpns.len() + qmux::ALPNS.len());
	for &alpn in alpns {
		for version in versions {
			if version == qmux::Version::QMux00 && alpn == "moqt-18" {
				continue;
			}
			protocols.push(format!("{}{alpn}", version.prefix()));
		}
	}
	protocols.extend(qmux::ALPNS.iter().map(|s| s.to_string()));
	protocols
}

impl Error {
	pub(crate) fn connect_error(&self) -> Option<crate::ConnectError> {
		match self {
			Self::ConnectRejected(err) => Some(*err),
			_ => None,
		}
	}
}

fn map_websocket_error(err: tungstenite::Error) -> Error {
	if let tungstenite::Error::Http(response) = &err
		&& let Some(err) = crate::ConnectError::from_status_u16(response.status().as_u16())
	{
		return err.into();
	}

	Error::WebSocketConnect(err)
}

/// Listens for incoming WebSocket connections on a TCP port.
///
/// Use with [`crate::Server::with_websocket`] to accept WebSocket connections
/// alongside QUIC connections on a separate port.
pub struct Listener {
	listener: tokio::net::TcpListener,
	server: qmux::Server,
}

impl Listener {
	pub async fn bind(addr: net::SocketAddr) -> Result<Self> {
		Self::bind_with_alpns(addr, moq_net::ALPNS).await
	}

	pub async fn bind_with_alpns(addr: net::SocketAddr, alpns: &[&str]) -> Result<Self> {
		let listener = tokio::net::TcpListener::bind(addr).await?;
		// Empty version slice = every QMux draft qmux knows about.
		let any: &[qmux::Version] = &[];
		let server = qmux::Server::new().with_protocols(alpns.iter().map(|&alpn| (alpn, any)));
		Ok(Self { listener, server })
	}

	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	pub async fn accept(&self) -> Option<Result<qmux::Session>> {
		match self.listener.accept().await {
			Ok((stream, addr)) => {
				tracing::debug!(%addr, "accepted WebSocket TCP connection");
				let server = self.server.clone();
				Some(server.accept(stream).await.map_err(Error::Accept))
			}
			Err(e) => Some(Err(e.into())),
		}
	}
}
