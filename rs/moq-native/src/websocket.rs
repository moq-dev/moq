//! WebSocket fallback transport, running the QMux wire format over `ws://` or `wss://`.
//!
//! Used when QUIC is unreachable: UDP blocked by a firewall, a proxy in the way, a
//! network that only passes TCP/443. The client races this against QUIC and gives QUIC
//! a small head start ([`Client::delay`]), so WebSocket only wins when QUIC can't get
//! through. Servers accept it on a separate TCP port via [`Listener`].

use qmux::tokio_tungstenite;
use qmux::tokio_tungstenite::tungstenite::{self, http};
use std::collections::HashSet;
use std::sync::{Arc, LazyLock, Mutex};
use std::{net, time};
use url::Url;

/// Errors specific to the WebSocket fallback backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The TCP socket failed to bind, accept, or connect.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// WebSocket fallback was turned off via [`Client::enabled`].
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

	// Most moq ALPNs can ride on any QMux draft (`&[]` lets the polyfill expand
	// to every version it knows). `qmux_versions_for` pins the few that the spec
	// restricts. qmux also offers the bare ALPNs (`qmux-01`, `qmux-00`,
	// `webtransport`) by default so we still interop with relays that only know a
	// wire-format version.
	let session = qmux::Client::new()
		.with_protocols(alpns.iter().map(|&a| (a, qmux_versions_for(a))))
		.with_connector(connector)
		.with_keep_alive(qmux::KeepAlive::default()) // 5s ping / 30s deadline, parity with QUIC
		.connect(url.as_str())
		.await
		.map_err(Error::Connect)?;

	tracing::warn!(%url, "using WebSocket fallback");
	WEBSOCKET_WON.lock().unwrap().insert(key);

	Ok(session)
}

/// The QMux drafts a moq ALPN is allowed to ride on, for `qmux::*::with_protocols`.
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
	/// Bind a listener to the given address, accepting every moq ALPN we know about.
	pub async fn bind(addr: net::SocketAddr) -> Result<Self> {
		Self::bind_with_alpns(addr, moq_net::ALPNS).await
	}

	/// Bind a listener that only accepts the given moq ALPNs, in preference order.
	pub async fn bind_with_alpns(addr: net::SocketAddr, alpns: &[&str]) -> Result<Self> {
		let listener = tokio::net::TcpListener::bind(addr).await?;
		// `qmux_versions_for` returns `&[]` (every QMux draft) for ALPNs the spec
		// doesn't restrict; qmux by default also accepts legacy clients that
		// only offer a bare wire-format ALPN (today's moq-net clients still do).
		let server = qmux::Server::new().with_protocols(alpns.iter().map(|&a| (a, qmux_versions_for(a))));
		Ok(Self { listener, server })
	}

	/// The local address the listener is bound to.
	pub fn local_addr(&self) -> Result<net::SocketAddr> {
		Ok(self.listener.local_addr()?)
	}

	/// Accept the next connection, performing the WebSocket upgrade and qmux handshake.
	///
	/// Returns `None` only if the listener itself is gone; a per-connection failure is
	/// yielded as `Some(Err(..))` so the accept loop keeps running.
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

#[cfg(test)]
mod tests {
	use super::*;

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
