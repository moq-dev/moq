use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::protocol::validate_protocol;
use crate::transport::WsTransport;
use moq_net::transport::Address;
use moq_quic::Side;
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::{Config, Driver, Error, Session, Version, alpn};

/// The largest WebSocket message or frame a session under `config` reads.
///
/// Each message carries one record, or one frame on draft-00, so the socket is capped
/// at this: a larger one would be refused anyway, but only after buffering it whole.
fn max_message_size(config: &Config) -> usize {
	usize::try_from(config.max_record_size)
		.unwrap_or(usize::MAX)
		.max(crate::MAX_FRAME_SIZE)
}

/// The socket settings for a session under `session`: messages and frames held to
/// [`max_message_size`], read in chunks of one default record.
///
/// The transport copies each message out, so the read buffer never backs a record. tungstenite
/// allocates it up front and zero-fills it on every read, so it stays one default record
/// rather than its 128 KiB default or a large record limit; a larger message grows it.
pub(crate) fn capped(session: &Config) -> tungstenite::protocol::WebSocketConfig {
	let max = max_message_size(session);
	tungstenite::protocol::WebSocketConfig::default()
		.max_message_size(Some(max))
		.max_frame_size(Some(max))
		.read_buffer_size(crate::transport::READ_BUFFER)
}

/// Keep-alive configuration for WebSocket transports.
///
/// WebSocket has no built-in idle timeout: when the peer's host crashes
/// or its network drops without sending a TCP FIN, the local socket
/// stays "open" until OS-level TCP keep_alive eventually probes - typically
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
		// Match the QUIC defaults used by moq-tokio: 5s ping, 30s deadline.
		Self {
			interval: Duration::from_secs(5),
			timeout: Duration::from_secs(30),
		}
	}
}

/// Run a QMux session over a connection whose HTTP upgrade to WebSocket already
/// finished.
///
/// Use this when an HTTP server (e.g. hyper or axum) answered the upgrade and the
/// caller has the raw upgraded IO and the negotiated `Sec-WebSocket-Protocol`
/// value, which names the QMux wire-format version and application protocol.
/// [`connect`](Self::connect) speaks the WebSocket client role and
/// [`accept`](Self::accept) the server role. The IO must hold nothing but
/// WebSocket frames: no handshake bytes, and none already read past it. Disable
/// Nagle on the TCP socket underneath, as with [`Server::accept`].
pub struct Upgraded<T> {
	io: T,
	alpn: String,
	keep_alive: Option<KeepAlive>,
	socket_stats: Option<crate::SharedSocketStats>,
	session: Config,
}

impl<T: AsyncRead + AsyncWrite + Unpin> Upgraded<T> {
	/// Wrap `io`, whose upgrade negotiated the `Sec-WebSocket-Protocol` value `alpn`.
	pub fn new(io: T, alpn: &str) -> Self {
		Self {
			io,
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
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Report statistics for the socket underneath the upgrade.
	///
	/// An HTTP server hands over a type-erased upgraded stream, so QMux can't find
	/// the socket itself. A server that kept a handle on the accepted connection
	/// can attach it here and get the kernel's RTT (and, on Linux, delivery rate and byte counts)
	/// instead of only QMux's own `QX_PING` measurement, and keep the kernel's unsent
	/// queue to about one congestion window. See `TcpStats` (Linux and macOS).
	pub fn with_socket_stats(mut self, stats: crate::SharedSocketStats) -> Self {
		self.socket_stats = Some(stats);
		self
	}

	/// Wrap as a client-side session once it is established, returned with the
	/// [`Driver`] to spawn.
	///
	/// As with [`Session::connect`], established means the peer's transport parameters
	/// arrived, so its stream and datagram limits are known. Fails when the subprotocol
	/// names no QMux version, or with [`Error::HandshakeTimeout`] if the parameters never
	/// arrive.
	pub async fn connect(self) -> Result<(Session, Driver<WsTransport<WebSocketStream<T>>>), Error> {
		self.start(Side::Client).await
	}

	/// Wrap as a server-side session once it is established, as with
	/// [`connect`](Self::connect).
	pub async fn accept(self) -> Result<(Session, Driver<WsTransport<WebSocketStream<T>>>), Error> {
		self.start(Side::Server).await
	}

	async fn start(self, side: Side) -> Result<(Session, Driver<WsTransport<WebSocketStream<T>>>), Error> {
		let (version, protocol) = alpn::parse(Some(&self.alpn))?;
		let config = self.session.clone().with_negotiated(version, protocol);
		// Before any bytes go out, so a bad config never reaches the peer.
		config.to_mux()?;
		let role = match side {
			Side::Client => tungstenite::protocol::Role::Client,
			Side::Server => tungstenite::protocol::Role::Server,
		};
		let ws = WebSocketStream::from_raw_socket(self.io, role, Some(capped(&config))).await;
		let mut transport = WsTransport::new(ws, Address::Uri);
		if let Some(stats) = self.socket_stats {
			transport = transport.with_socket_stats(stats);
		}
		let transport = match self.keep_alive {
			Some(ka) => transport.with_keep_alive(ka)?,
			None => transport,
		};
		Session::start(transport, side, config).await
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
	keep_alive: Option<KeepAlive>,
	session: Config,
	tls: Option<std::sync::Arc<rustls::ClientConfig>>,
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
	/// sizes are held to one record of `config`.
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Dial `wss://` URLs with this TLS policy instead of the platform's verifier.
	///
	/// Without it, `wss://` certificates are checked by the OS trust store through
	/// `rustls-platform-verifier`, which on Android the app must initialize first.
	pub fn with_tls(mut self, tls: std::sync::Arc<rustls::ClientConfig>) -> Self {
		self.tls = Some(tls);
		self
	}

	/// Connect to a WebSocket server, negotiating an advertised `(alpn, version)`, and
	/// return the session once it is established.
	pub async fn connect(
		&self,
		url: &str,
	) -> Result<
		(
			Session,
			Driver<WsTransport<WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>>>,
		),
		Error,
	> {
		use tungstenite::{client::IntoClientRequest, http};

		for (a, _) in &self.protocols {
			validate_protocol(a)?;
		}
		let deadline = self.session.handshake_deadline();

		let mut request = url.into_client_request().map_err(Error::websocket)?;

		let entries = self.protocols.iter().map(|(a, vs)| (a.as_str(), vs.as_slice()));
		let offered = alpn::build(entries, self.require_protocol);
		// Refuse a config any offered version would refuse before dialing, so it fails here
		// rather than after the server picked that version.
		self.session.check_offered(&offered)?;
		let protocol_value = offered.join(", ");

		request.headers_mut().insert(
			http::header::SEC_WEBSOCKET_PROTOCOL,
			http::HeaderValue::from_str(&protocol_value).map_err(|_| Error::InvalidProtocol(protocol_value))?,
		);

		let ws_config = capped(&self.session);

		// `true` disables Nagle: records are already batched, see `Server::accept`.
		let (ws_stream, response) = {
			let connector = match (
				&self.tls,
				request
					.uri()
					.scheme_str()
					.is_some_and(|scheme| scheme.eq_ignore_ascii_case("wss")),
			) {
				(Some(tls), _) => Some(tokio_tungstenite::Connector::Rustls(tls.clone())),
				(None, true) => Some(tokio_tungstenite::Connector::Rustls(platform_tls()?)),
				(None, false) => None,
			};
			let upgrade = tokio_tungstenite::connect_async_tls_with_config(request, Some(ws_config), true, connector);
			crate::config::within(deadline, upgrade)
				.await?
				.map_err(Error::websocket)?
		};

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

		let config = self.session.clone().with_negotiated(version, protocol).until(deadline);
		let tcp = match ws_stream.get_ref() {
			MaybeTlsStream::Plain(tcp) => Some(tcp),
			MaybeTlsStream::Rustls(tls) => Some(tls.get_ref().0),
			_ => None,
		};
		let socket_stats = tcp.map(crate::tcp::adopt).transpose()?.flatten();
		let mut transport = WsTransport::new(ws_stream, Address::Uri);
		if let Some(stats) = socket_stats {
			transport = transport.with_socket_stats(stats);
		}
		let transport = match self.keep_alive {
			Some(ka) => transport.with_keep_alive(ka)?,
			None => transport,
		};
		Session::start(transport, Side::Client, config).await
	}
}

/// The TLS config for `wss://` dials given none, verifying with the OS trust store, built once
/// since the verifier is not free to build.
fn platform_tls() -> Result<std::sync::Arc<rustls::ClientConfig>, Error> {
	use rustls_platform_verifier::BuilderVerifierExt;
	static TLS: std::sync::OnceLock<std::sync::Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
	if let Some(tls) = TLS.get() {
		return Ok(tls.clone());
	}
	let provider = moq_crypto::provider::provider()?;
	let config = rustls::ClientConfig::builder_with_provider(provider)
		.with_safe_default_protocol_versions()
		.map_err(std::io::Error::other)?
		.with_platform_verifier()
		.map_err(std::io::Error::other)?
		.with_no_client_auth();
	Ok(TLS.get_or_init(|| std::sync::Arc::new(config)).clone())
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
	/// sizes are held to one record of `config`.
	pub fn with_session_config(mut self, config: Config) -> Self {
		self.session = config;
		self
	}

	/// Accept a WebSocket connection, negotiating an offered `(alpn, version)`, and
	/// return the session once it is established.
	///
	/// Disable Nagle on a TCP `socket` first (`set_nodelay(true)`): records are
	/// already batched, so Nagle would only hold a batch's tail for the peer's ACK.
	pub async fn accept<T: AsyncRead + AsyncWrite + Unpin>(
		&self,
		socket: T,
	) -> Result<(Session, Driver<WsTransport<WebSocketStream<T>>>), Error> {
		use std::sync::{Arc, Mutex};
		use tungstenite::{handshake::server, http};

		for (a, _) in &self.protocols {
			validate_protocol(a)?;
		}
		let deadline = self.session.handshake_deadline();

		// The version and protocol picked, or why the session config refuses them
		type Negotiated = Result<(Version, Option<String>), Error>;
		let negotiated = Arc::new(Mutex::new(None::<Negotiated>));
		let negotiated_clone = negotiated.clone();
		let supported = self.protocols.clone();
		let require_protocol = self.require_protocol;
		let session = self.session.clone();

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
			let prefixed = supported.iter().find_map(|(alpn, versions)| {
				alpn::expand_versions(versions).iter().find_map(|&version| {
					let wire = format!("{}{}", version.prefix(), alpn);
					header_protocols
						.contains(&wire.as_str())
						.then(|| (wire, version, Some(alpn.clone())))
				})
			});
			// Default fallback: accept bare version ALPNs (qmux-02, qmux-01,
			// qmux-00) for clients that didn't request an app protocol, unless
			// require_protocol was set. The session ends up with
			// `protocol = None` and the wire-format version implied by
			// whichever bare ALPN won.
			let bare = || {
				Version::ALL
					.iter()
					.find(|version| !require_protocol && header_protocols.contains(&version.alpn()))
					.map(|&version| (version.alpn().to_string(), version, None))
			};
			let Some((wire, version, protocol)) = prefixed.or_else(bare) else {
				return Err(http::Response::builder()
					.status(http::StatusCode::BAD_REQUEST)
					.body(Some("no supported protocol".to_string()))
					.unwrap());
			};

			// Refuse a config the picked version cannot run before the upgrade completes,
			// so the client hears why instead of a dropped socket.
			if let Err(err) = session.clone().with_negotiated(version, None).to_mux() {
				*negotiated_clone.lock().unwrap() = Some(Err(err));
				return Err(http::Response::builder()
					.status(http::StatusCode::INTERNAL_SERVER_ERROR)
					.body(Some("invalid session config".to_string()))
					.unwrap());
			}
			response.headers_mut().insert(
				http::header::SEC_WEBSOCKET_PROTOCOL,
				http::HeaderValue::from_str(&wire).unwrap(),
			);
			*negotiated_clone.lock().unwrap() = Some(Ok((version, protocol)));
			Ok(response)
		};

		let upgrade = tokio_tungstenite::accept_hdr_async_with_config(socket, callback, Some(capped(&self.session)));
		let ws = crate::config::within(deadline, upgrade).await?;
		let negotiated = negotiated.lock().unwrap().take();
		let (ws, (version, protocol)) = match (ws, negotiated) {
			(_, Some(Err(err))) => return Err(err),
			(Err(err), _) => return Err(Error::websocket(err)),
			(Ok(ws), Some(Ok(negotiated))) => (ws, negotiated),
			(Ok(_), None) => unreachable!("an upgrade completes only once the callback negotiated"),
		};

		let config = self.session.clone().with_negotiated(version, protocol).until(deadline);
		let transport = WsTransport::new(ws, Address::Uri);
		let transport = match self.keep_alive {
			Some(ka) => transport.with_keep_alive(ka)?,
			None => transport,
		};
		Session::start(transport, Side::Server, config).await
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use tokio::io::AsyncWriteExt;

	/// A WebSocket over loopback TCP counts the kernel's bytes: at least the payload each way,
	/// read once the session ends and its driver publishes the last count. It waits on the
	/// driver, not a clock, since the kernel's counters are real.
	#[cfg(target_os = "linux")]
	#[tokio::test]
	async fn a_websocket_counts_the_tcp_bytes_it_moved() {
		use moq_net::transport::poll::{RecvStream as _, SendOptions, SendStream as _, Session as _};

		const PAYLOAD: usize = 100_000;

		async fn send(session: &Session) {
			let mut send = session.open_uni(SendOptions::default()).await.unwrap();
			let mut data = bytes::Bytes::from(vec![7; PAYLOAD]);
			while !data.is_empty() {
				send.write_buf(&mut data).await.unwrap();
			}
			send.finish().unwrap();
		}

		async fn receive(session: &Session) {
			let mut recv = session.accept_uni().await.unwrap();
			let mut read = 0;
			while let Some(chunk) = recv.read_chunk(usize::MAX).await.unwrap() {
				read += chunk.len();
			}
			assert_eq!(read, PAYLOAD);
		}

		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("ws://{}", listener.local_addr().unwrap());
		let client = Client::new();
		let (client, server) = tokio::join!(client.connect(&url), async {
			let (socket, _) = listener.accept().await.unwrap();
			Server::new().accept(socket).await
		});
		let (client, client_driver) = client.unwrap();
		let (server, server_driver) = server.unwrap();
		let client_driver = tokio::spawn(client_driver);
		tokio::spawn(server_driver);

		tokio::join!(send(&client), receive(&server));
		tokio::join!(send(&server), receive(&client));
		client.close(0, "done");
		client_driver.await.unwrap();

		let wire = client.wire().unwrap();
		assert!(wire.sent_bytes() >= PAYLOAD as u64, "sent {}", wire.sent_bytes());
		assert!(
			wire.received_bytes() >= PAYLOAD as u64,
			"received {}",
			wire.received_bytes()
		);
	}

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
		// A masked binary frame declaring 1 MiB, with none of its payload sent, before the
		// client's transport parameters: the accept fails on it rather than awaiting them.
		let mut frame = vec![0x82, 0x80 | 127];
		frame.extend_from_slice(&(1u64 << 20).to_be_bytes());
		frame.extend_from_slice(&[0; 4]);
		client.write_all(&frame).await.unwrap();

		let accepted = kio::clock::timeout(Duration::from_secs(1), accept)
			.await
			.expect("the socket waited to buffer a frame larger than a record")
			.unwrap();
		assert!(
			matches!(&accepted, Err(err) if !matches!(err, Error::HandshakeTimeout)),
			"{:?}",
			accepted.map(|_| ())
		);
	}

	/// A `wss://` dial without a TLS config checks the server against the OS trust store, which
	/// refuses a self-signed certificate, and builds that check once.
	#[tokio::test]
	async fn wss_verifies_with_the_platform_by_default() {
		use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

		let tls = platform_tls().unwrap();
		assert!(std::sync::Arc::ptr_eq(&tls, &platform_tls().unwrap()), "built again");

		let signing_key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
		let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
		params.serial_number = Some(1u64.into());
		let cert = params.self_signed(&signing_key).unwrap();
		let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
			signing_key.to_pkcs8().secret_pkcs8_der().to_vec(),
		));
		let server = rustls::ServerConfig::builder_with_provider(moq_crypto::provider::provider().unwrap())
			.with_safe_default_protocol_versions()
			.unwrap()
			.with_no_client_auth()
			.with_single_cert(vec![CertificateDer::from(cert.der().to_vec())], key)
			.unwrap();
		let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(server));

		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("wss://localhost:{}", listener.local_addr().unwrap().port());
		let client = Client::new();
		let (dialed, accepted) = tokio::join!(client.connect(&url), async {
			let (socket, _) = listener.accept().await.unwrap();
			acceptor.accept(socket).await
		});
		assert!(dialed.is_err(), "trusted a self-signed certificate");
		assert!(accepted.is_err(), "the client finished the TLS handshake");
	}

	/// A server that answers with a subprotocol the client never offered is refused before
	/// the session starts, even one naming a QMux version.
	#[tokio::test]
	async fn an_unoffered_subprotocol_is_refused() {
		use tokio::io::AsyncReadExt;

		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("ws://{}", listener.local_addr().unwrap());
		let client = Client::new()
			.with_protocol("moq-lite-03", &[Version::QMux02])
			.require_protocol();
		let server = async {
			let (mut socket, _) = listener.accept().await.unwrap();
			let mut request = Vec::new();
			while !request.ends_with(b"\r\n\r\n") {
				let mut byte = [0];
				socket.read_exact(&mut byte).await.unwrap();
				request.push(byte[0]);
			}
			let request = String::from_utf8(request).unwrap();
			let key = request
				.lines()
				.find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
				.or_else(|| {
					request
						.lines()
						.find_map(|line| line.strip_prefix("sec-websocket-key: "))
				})
				.unwrap();
			let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
			let response = format!(
				"HTTP/1.1 101 Switching Protocols\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
				Sec-WebSocket-Accept: {accept}\r\nSec-WebSocket-Protocol: qmux-01.moq-lite-03\r\n\r\n"
			);
			socket.write_all(response.as_bytes()).await.unwrap();
			// A client that took the answer sends its transport parameters next.
			let mut byte = [0];
			socket.read(&mut byte).await.unwrap()
		};
		tokio::select! {
			biased;
			dialed = client.connect(&url) => assert!(dialed.is_err(), "took an unoffered subprotocol"),
			read = server => panic!("the client started a session, sending {read} bytes"),
		}
	}

	const UPGRADE: &[u8] = b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
		Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
		Sec-WebSocket-Protocol: qmux-02\r\n\r\n";

	/// The upgrade and the wait for the peer's parameters share one handshake deadline: a
	/// client that never finishes its upgrade times out, and one that takes most of the
	/// deadline to upgrade has only the rest to send its parameters.
	#[tokio::test(start_paused = true)]
	async fn the_upgrade_and_the_parameters_share_one_deadline() {
		let timeout = Config::default().handshake_timeout.unwrap();

		let (_client, server) = tokio::io::duplex(1 << 16);
		let start = kio::clock::now();
		let accepted = Server::new().accept(server).await;
		assert!(matches!(accepted, Err(Error::HandshakeTimeout)), "a stalled upgrade");
		assert_eq!(kio::clock::now() - start, timeout);

		let (mut client, server) = tokio::io::duplex(1 << 16);
		let start = kio::clock::now();
		let accept = tokio::spawn(async move { Server::new().accept(server).await.map(|_| ()) });
		kio::clock::sleep(timeout - Duration::from_secs(1)).await;
		client.write_all(UPGRADE).await.unwrap();
		let accepted = accept.await.unwrap();
		assert!(matches!(accepted, Err(Error::HandshakeTimeout)), "no parameters");
		assert_eq!(kio::clock::now() - start, timeout);
	}

	/// A session config the negotiated version refuses fails before the upgrade completes, so
	/// the client gets an HTTP error instead of a socket that drops after it.
	#[tokio::test(start_paused = true)]
	async fn an_invalid_config_fails_before_the_upgrade() {
		let config = Config {
			max_record_size: 1000,
			..Config::default()
		};
		let (mut client, server) = tokio::io::duplex(1 << 16);
		let accept = tokio::spawn(async move {
			Server::new()
				.with_session_config(config)
				.accept(server)
				.await
				.map(|_| ())
		});
		client.write_all(UPGRADE).await.unwrap();
		assert!(matches!(accept.await.unwrap(), Err(Error::InvalidConfig(_))));
		let mut response = String::new();
		tokio::io::AsyncReadExt::read_to_string(&mut client, &mut response)
			.await
			.unwrap();
		assert!(response.starts_with("HTTP/1.1 500"), "{response}");

		// A client refuses before it dials, here an address nothing listens on.
		let client = Client::new().with_session_config(Config {
			max_record_size: 1000,
			..Config::default()
		});
		let dialed = client.connect("ws://127.0.0.1:1").await;
		assert!(
			matches!(dialed, Err(Error::InvalidConfig(_))),
			"{:?}",
			dialed.map(|_| ())
		);
	}

	/// A large record limit bounds messages, not the read buffer tungstenite allocates and
	/// zero-fills on every read.
	#[test]
	fn a_large_record_limit_keeps_the_read_buffer_small() {
		let config = Config {
			max_record_size: 64 << 20,
			..Config::default()
		};
		let ws = capped(&config);
		assert_eq!(ws.read_buffer_size, crate::transport::READ_BUFFER);
		assert_eq!(ws.max_message_size, Some(64 << 20));
	}
}
