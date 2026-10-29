use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use moq_net::transport::Address;
use moq_quic::Side;

use crate::{Config, Error, Session, Version, alpn};

/// The `tokio-tungstenite` version this crate is built against.
///
/// [`Client::with_connector`] and [`Upgraded`] take `tokio-tungstenite` types
/// directly, so use this re-export rather than a separately pinned dependency —
/// a different minor version is a distinct crate to the compiler and fails to
/// typecheck against these APIs.
pub use tokio_tungstenite;

/// The `tungstenite` version this crate is built against, matching
/// [`tokio_tungstenite`].
///
/// [`Client::with_config`] and [`Upgraded`] name `tungstenite` types, and
/// [`Error::WebSocket`] wraps a `tungstenite::Error`.
pub use tokio_tungstenite::tungstenite;

/// The largest WebSocket message or frame a session under `config` reads.
///
/// Each message carries one record, or one frame on draft-00, so cap a socket upgraded
/// elsewhere (see [`Upgraded`]) at this: a larger one is refused anyway, but only after
/// the socket buffered it whole.
pub fn max_message_size(config: &Config) -> usize {
	usize::try_from(config.max_record_size)
		.unwrap_or(usize::MAX)
		.max(crate::MAX_FRAME_SIZE)
}

/// `config` with its message and frame sizes held to [`max_message_size`] of `session`.
fn capped(
	config: Option<tungstenite::protocol::WebSocketConfig>,
	session: &Config,
) -> tungstenite::protocol::WebSocketConfig {
	let max = max_message_size(session);
	let mut config = config.unwrap_or_default();
	config.max_message_size = Some(config.max_message_size.map_or(max, |size| size.min(max)));
	config.max_frame_size = Some(config.max_frame_size.map_or(max, |size| size.min(max)));
	config
}

/// Keep-alive configuration for WebSocket transports.
///
/// WebSocket has no built-in idle timeout: when the peer's host crashes
/// or its network drops without sending a TCP FIN, the local socket
/// stays "open" until OS-level TCP keep_alive eventually probes — typically
/// hours. Set this to send periodic Pings and close the session if no
/// frame arrives within `timeout`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct KeepAlive {
	/// How often to send a Ping frame to the peer.
	pub interval: Duration,

	/// Close the session if no frame is received from the peer within this window.
	/// Should be a small multiple of `interval` to tolerate transient drops; a session
	/// refuses one no longer than `interval` with [`Error::InvalidConfig`].
	pub timeout: Duration,
}

impl KeepAlive {
	/// Create a keep-alive config with the given interval and timeout.
	pub fn new(interval: Duration, timeout: Duration) -> Self {
		Self { interval, timeout }
	}
}

impl Default for KeepAlive {
	fn default() -> Self {
		// Match the QUIC defaults used by moq-native: 5s ping, 30s deadline.
		Self {
			interval: Duration::from_secs(5),
			timeout: Duration::from_secs(30),
		}
	}
}

/// Wrap an already-upgraded WebSocket as a QMux session.
///
/// Use this when the WebSocket handshake was performed by an external
/// framework (e.g. axum) and the caller already has the negotiated
/// `Sec-WebSocket-Protocol` value, which names the QMux wire-format version and
/// application protocol. Disable Nagle on the TCP socket underneath, as with
/// [`Server::accept`].
pub struct Upgraded<T> {
	ws: T,
	alpn: String,
	keep_alive: Option<KeepAlive>,
	socket_stats: Option<crate::SharedSocketStats>,
	session: Config,
}

impl<T> Upgraded<T>
where
	T: futures::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
		+ futures::Sink<tungstenite::Message, Error = tungstenite::Error>
		+ Unpin
		+ Send
		+ 'static,
{
	/// Wrap `ws`, whose handshake negotiated the `Sec-WebSocket-Protocol` value `alpn`.
	pub fn new(ws: T, alpn: &str) -> Self {
		Self {
			ws,
			alpn: alpn.to_string(),
			keep_alive: None,
			socket_stats: None,
			session: Config::default(),
		}
	}

	/// Drive a keep-alive Ping/timeout on the WebSocket.
	pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
		self.keep_alive = Some(keep_alive);
		self
	}

	/// Use `config` for every session setting except the wire-format version
	/// and application protocol, which the handshake negotiates.
	///
	/// The WebSocket handshake already settled the protocol, so the session starts
	/// without awaiting the peer's transport parameters, failing later with
	/// [`Error::HandshakeTimeout`] if they never arrive. Cap the socket's message and
	/// frame sizes at [`max_message_size`] of `config`, since each record is one message.
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Report statistics for the socket underneath the upgrade.
	///
	/// An HTTP server hands over a type-erased upgraded stream, so QMux can't find
	/// the socket itself. A server that kept a handle on the accepted connection
	/// can attach it here and get the kernel's RTT (and, on Linux, delivery rate)
	/// instead of only QMux's own `QX_PING` measurement. See
	/// `TcpStats` (Linux and macOS).
	pub fn with_socket_stats(mut self, stats: crate::SharedSocketStats) -> Self {
		self.socket_stats = Some(stats);
		self
	}

	/// Wrap as a client-side session.
	///
	/// The protocol is already known from the negotiated subprotocol (ALPN), so
	/// this returns synchronously without awaiting in-band parameters. Fails when
	/// the subprotocol names no QMux version.
	pub fn connect(self) -> Result<Session, Error> {
		self.start(Side::Client)
	}

	/// Wrap as a server-side session.
	///
	/// As with [`connect`](Self::connect), the protocol is known from the
	/// negotiated subprotocol, so this returns synchronously.
	pub fn accept(self) -> Result<Session, Error> {
		self.start(Side::Server)
	}

	fn start(self, side: Side) -> Result<Session, Error> {
		let (version, protocol) = alpn::parse(Some(&self.alpn))?;
		let config = self.session.clone().with_negotiated(version, protocol);
		let transport = self.into_transport()?;
		Session::new(transport, side, config)
	}

	fn into_transport(self) -> Result<WsTransport<T>, Error> {
		let mut transport = WsTransport::new(self.ws, Address::Uri);
		if let Some(stats) = self.socket_stats {
			transport = transport.with_socket_stats(stats);
		}
		Ok(match self.keep_alive {
			Some(ka) => transport.with_keep_alive(ka)?,
			None => transport,
		})
	}
}

/// A QMux client that opens a WebSocket and negotiates an application protocol.
///
/// Each entry pairs an `alpn` with the QMux wire-format `versions` it can
/// ride on. The wire form `{v.prefix()}{alpn}` is emitted once per listed
/// version, in order; an empty `versions` slice expands to every QMux draft
/// this crate knows about. Add multiple entries to negotiate across drafts in
/// a single handshake; the wire-format version comes from the negotiated
/// subprotocol. By default the bare version ALPNs (`qmux-02`, `qmux-01`,
/// `qmux-00`) are also offered for peers that don't pin an app protocol;
/// call [`Client::require_protocol`] to offer only the configured pairs.
#[derive(Default, Clone)]
pub struct Client {
	protocols: Vec<(String, Vec<Version>)>,
	require_protocol: bool,
	config: Option<tungstenite::protocol::WebSocketConfig>,
	keep_alive: Option<KeepAlive>,
	session: Config,
	#[cfg(feature = "wss")]
	connector: Option<tokio_tungstenite::Connector>,
}

impl Client {
	pub fn new() -> Self {
		Self::default()
	}

	/// Advertise `alpn` under the listed QMux wire-format versions.
	///
	/// Pass `&[v]` to pin to one version, `&[v1, v2]` to enumerate, or `&[]`
	/// to let the polyfill expand to every QMux draft it knows about.
	pub fn with_protocol(mut self, alpn: &str, versions: &[Version]) -> Self {
		self.protocols.push((alpn.to_string(), versions.to_vec()));
		self
	}

	/// Advertise multiple `(alpn, versions)` entries in preference order.
	pub fn with_protocols<'a>(mut self, entries: impl IntoIterator<Item = (&'a str, &'a [Version])>) -> Self {
		self.protocols
			.extend(entries.into_iter().map(|(a, vs)| (a.to_string(), vs.to_vec())));
		self
	}

	/// Offer only the prefixed `(alpn, version)` pairs, suppressing the bare
	/// version ALPNs (`qmux-02`, `qmux-01`, `qmux-00`) that are offered by
	/// default. Use this when the peer must commit to one of the application
	/// protocols you've configured.
	pub fn require_protocol(mut self) -> Self {
		self.require_protocol = true;
		self
	}

	/// Set the WebSocket configuration, its message and frame sizes held to [`max_message_size`].
	pub fn with_config(mut self, config: tungstenite::protocol::WebSocketConfig) -> Self {
		self.config = Some(config);
		self
	}

	/// Send periodic Pings and close the session if the peer goes silent.
	///
	/// WebSocket has no built-in idle timeout, so without this a crashed peer
	/// stays "connected" until OS-level TCP keep_alive eventually probes.
	pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
		self.keep_alive = Some(keep_alive);
		self
	}

	/// Use `config` for every session setting except the wire-format version
	/// and application protocol, which the handshake negotiates.
	///
	/// The WebSocket handshake already settled the protocol, so the session starts
	/// without awaiting the peer's transport parameters, failing later with
	/// [`Error::HandshakeTimeout`] if they never arrive. The socket's message and frame
	/// sizes are held to [`max_message_size`] of `config`, since each record is one message.
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Set the TLS connector for secure WebSocket connections.
	#[cfg(feature = "wss")]
	pub fn with_connector(mut self, connector: tokio_tungstenite::Connector) -> Self {
		self.connector = Some(connector);
		self
	}

	/// Connect to a WebSocket server, negotiating an advertised `(alpn, version)`.
	pub async fn connect(&self, url: &str) -> Result<Session, Error> {
		use tungstenite::{client::IntoClientRequest, http};

		for (a, _) in &self.protocols {
			validate_protocol(a)?;
		}

		let mut request = url.into_client_request().map_err(Error::from)?;

		let entries = self.protocols.iter().map(|(a, vs)| (a.as_str(), vs.as_slice()));
		let protocol_value = alpn::build(entries, self.require_protocol).join(", ");

		request.headers_mut().insert(
			http::header::SEC_WEBSOCKET_PROTOCOL,
			http::HeaderValue::from_str(&protocol_value).map_err(|_| Error::InvalidProtocol(protocol_value))?,
		);

		let ws_config = capped(self.config, &self.session);

		// `true` disables Nagle: records are already batched, see `Server::accept`.
		#[cfg(feature = "wss")]
		let (ws_stream, response) = {
			tokio_tungstenite::connect_async_tls_with_config(request, Some(ws_config), true, self.connector.clone())
				.await
				.map_err(Error::from)?
		};

		#[cfg(not(feature = "wss"))]
		let (ws_stream, response) = tokio_tungstenite::connect_async_with_config(request, Some(ws_config), true)
			.await
			.map_err(Error::from)?;

		let negotiated = response
			.headers()
			.get(http::header::SEC_WEBSOCKET_PROTOCOL)
			.and_then(|h| h.to_str().ok());

		let (version, protocol) = alpn::parse(negotiated)?;

		// In strict mode a bare version subprotocol would silently drop the
		// application protocol we promised to enforce.
		if self.require_protocol && protocol.is_none() {
			return Err(Error::InvalidProtocol(negotiated.unwrap_or("<none>").to_string()));
		}

		let config = self.session.clone().with_negotiated(version, protocol);
		let transport = WsTransport::new(ws_stream, Address::Uri);
		let transport = match self.keep_alive {
			Some(ka) => transport.with_keep_alive(ka)?,
			None => transport,
		};
		// Protocol came from the negotiated subprotocol, so no in-band wait.
		Session::new(transport, Side::Client, config)
	}
}

/// A QMux server that accepts WebSocket connections.
///
/// Each entry pairs an `alpn` with the QMux wire-format `versions` it can
/// ride on. The handshake callback matches each `{v.prefix()}{alpn}`
/// permutation against the client's offered `Sec-WebSocket-Protocol` in
/// declaration order. An empty `versions` slice expands to every QMux draft
/// this crate knows about. By default bare version ALPNs (`qmux-02`,
/// `qmux-01`, `qmux-00`) are also accepted for peers that don't pin an
/// app protocol; call [`Server::require_protocol`] to accept only the
/// configured pairs.
#[derive(Default, Clone)]
pub struct Server {
	protocols: Vec<(String, Vec<Version>)>,
	require_protocol: bool,
	keep_alive: Option<KeepAlive>,
	session: Config,
}

impl Server {
	pub fn new() -> Self {
		Self::default()
	}

	/// Advertise `alpn` under the listed QMux wire-format versions.
	///
	/// Pass `&[v]` to support one version, `&[v1, v2]` to enumerate, or `&[]`
	/// to accept every QMux draft this crate knows about.
	pub fn with_protocol(mut self, alpn: &str, versions: &[Version]) -> Self {
		self.protocols.push((alpn.to_string(), versions.to_vec()));
		self
	}

	/// Advertise multiple `(alpn, versions)` entries in preference order.
	pub fn with_protocols<'a>(mut self, entries: impl IntoIterator<Item = (&'a str, &'a [Version])>) -> Self {
		self.protocols
			.extend(entries.into_iter().map(|(a, vs)| (a.to_string(), vs.to_vec())));
		self
	}

	/// Accept only the configured prefixed pairs, rejecting clients that offer
	/// just a bare version ALPN (`qmux-02`, `qmux-01`, `qmux-00`) with no
	/// application protocol. By default those bare ALPNs are accepted, yielding
	/// a session with `Config::protocol == None` and the wire-format version
	/// implied by the ALPN.
	pub fn require_protocol(mut self) -> Self {
		self.require_protocol = true;
		self
	}

	/// Send periodic Pings and close the session if the peer goes silent.
	///
	/// WebSocket has no built-in idle timeout, so without this a crashed peer
	/// stays "connected" until OS-level TCP keep_alive eventually probes.
	pub fn with_keep_alive(mut self, keep_alive: KeepAlive) -> Self {
		self.keep_alive = Some(keep_alive);
		self
	}

	/// Use `config` for every session setting except the wire-format version
	/// and application protocol, which the handshake negotiates.
	///
	/// The WebSocket handshake already settled the protocol, so the session starts
	/// without awaiting the peer's transport parameters, failing later with
	/// [`Error::HandshakeTimeout`] if they never arrive. The socket's message and frame
	/// sizes are held to [`max_message_size`] of `config`, since each record is one message.
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Accept a WebSocket connection, negotiating an offered `(alpn, version)`.
	///
	/// Disable Nagle on a TCP `socket` first (`set_nodelay(true)`): records are
	/// already batched, so Nagle would only hold a batch's tail for the peer's ACK.
	pub async fn accept<T: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
		&self,
		socket: T,
	) -> Result<Session, Error> {
		use std::sync::{Arc, Mutex};
		use tungstenite::{handshake::server, http};

		for (a, _) in &self.protocols {
			validate_protocol(a)?;
		}

		let negotiated = Arc::new(Mutex::new(None::<(Version, Option<String>)>));
		let negotiated_clone = negotiated.clone();
		let supported = self.protocols.clone();
		let require_protocol = self.require_protocol;

		#[expect(
			clippy::result_large_err,
			reason = "tungstenite's handshake callback fixes the error type"
		)]
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

			// Iterate supported entries in preference order; for each, expand
			// the listed versions (empty = every supported QMux draft) and pick
			// the first `{prefix}{alpn}` permutation the client offered.
			for (alpn, versions) in &supported {
				for &version in alpn::expand_versions(versions) {
					let wire = format!("{}{}", version.prefix(), alpn);
					if header_protocols.iter().any(|p| *p == wire) {
						response.headers_mut().insert(
							http::header::SEC_WEBSOCKET_PROTOCOL,
							http::HeaderValue::from_str(&wire).unwrap(),
						);
						*negotiated_clone.lock().unwrap() = Some((version, Some(alpn.clone())));
						return Ok(response);
					}
				}
			}

			// Default fallback: accept bare version ALPNs (qmux-02, qmux-01,
			// qmux-00) for clients that didn't request an app protocol, unless
			// require_protocol was set. The session ends up with
			// `protocol = None` and the wire-format version implied by
			// whichever bare ALPN won.
			if !require_protocol {
				for &version in Version::ALL {
					let bare = version.alpn();
					if header_protocols.contains(&bare) {
						response.headers_mut().insert(
							http::header::SEC_WEBSOCKET_PROTOCOL,
							http::HeaderValue::from_str(bare).unwrap(),
						);
						*negotiated_clone.lock().unwrap() = Some((version, None));
						return Ok(response);
					}
				}
			}

			Err(http::Response::builder()
				.status(http::StatusCode::BAD_REQUEST)
				.body(Some("no supported protocol".to_string()))
				.unwrap())
		};

		let ws = tokio_tungstenite::accept_hdr_async_with_config(socket, callback, Some(capped(None, &self.session)))
			.await?;

		let (version, protocol) = negotiated
			.lock()
			.unwrap()
			.take()
			.expect("negotiated must be set after successful handshake");

		let config = self.session.clone().with_negotiated(version, protocol);
		let transport = WsTransport::new(ws, Address::Uri);
		let transport = match self.keep_alive {
			Some(ka) => transport.with_keep_alive(ka)?,
			None => transport,
		};
		// Protocol came from the negotiated subprotocol, so no in-band wait.
		Session::new(transport, Side::Server, config)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use moq_net::transport::poll::Session as _;
	use tokio::io::AsyncWriteExt;

	/// A frame declaring more than one record is refused at its header rather than
	/// buffered whole, closing the session well before any timer would.
	#[tokio::test(start_paused = true)]
	async fn a_frame_past_one_record_is_refused_at_its_header() {
		let (mut client, server) = tokio::io::duplex(1 << 16);
		let accept = tokio::spawn(async move { Server::new().accept(server).await });
		client
			.write_all(
				b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
				Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
				Sec-WebSocket-Protocol: qmux-01\r\n\r\n",
			)
			.await
			.unwrap();
		let session = accept.await.unwrap().unwrap();

		// A masked binary frame declaring 1 MiB, with none of its payload sent.
		let mut frame = vec![0x82, 0x80 | 127];
		frame.extend_from_slice(&(1u64 << 20).to_be_bytes());
		frame.extend_from_slice(&[0; 4]);
		client.write_all(&frame).await.unwrap();

		kio::clock::timeout(Duration::from_secs(1), session.closed())
			.await
			.expect("the socket waited to buffer a frame larger than a record");
	}
}
