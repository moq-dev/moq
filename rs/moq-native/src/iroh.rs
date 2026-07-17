//! Iroh P2P transport, dialed by endpoint id instead of a hostname.
//!
//! A single [`Endpoint`] serves both roles, hole-punching directly to peers and
//! falling back to an iroh relay. Both WebTransport-over-H3 and raw QUIC are
//! negotiated via ALPN.

use std::{net, path::PathBuf, str::FromStr};

use url::Url;
use web_transport_iroh::iroh::{self, SecretKey};
// NOTE: web-transport-iroh should re-export proto like web-transport-quinn does.
use web_transport_proto::{ConnectRequest, ConnectResponse};

pub use iroh::Endpoint;
pub use web_transport_iroh;

/// Errors specific to the iroh P2P backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Reading or writing the secret key file failed.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// The configured secret was neither a valid hex key nor a readable key file.
	#[error("invalid iroh secret key")]
	Secret(#[source] iroh::KeyParsingError),

	/// The endpoint could not bind its UDP socket.
	#[error(transparent)]
	Bind(#[from] iroh::endpoint::BindError),

	/// A configured bind address was rejected by iroh.
	#[error(transparent)]
	BindAddr(#[from] iroh::endpoint::InvalidSocketAddr),

	/// Dialing the peer failed before a connection was started.
	#[error(transparent)]
	Connect(#[from] iroh::endpoint::ConnectWithOptsError),

	/// The QUIC handshake failed while connecting.
	#[error(transparent)]
	Connecting(#[from] iroh::endpoint::ConnectingError),

	/// The peer never settled on an ALPN.
	#[error(transparent)]
	Alpn(#[from] iroh::endpoint::AlpnError),

	/// An established connection was lost or closed.
	#[error(transparent)]
	Connection(#[from] iroh::endpoint::ConnectionError),

	/// The client side of the WebTransport handshake failed.
	#[error(transparent)]
	Client(#[from] web_transport_iroh::ClientError),

	/// The server side of the WebTransport handshake failed.
	#[error(transparent)]
	Server(#[from] web_transport_iroh::ServerError),

	/// The negotiated ALPN was not valid UTF-8.
	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::string::FromUtf8Error),

	/// The peer negotiated an ALPN this build does not speak.
	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	/// The URL had no host, so there is no endpoint id to dial.
	#[error("Invalid URL: missing host")]
	MissingHost,

	/// The URL host was not an iroh endpoint id. Unlike QUIC, iroh dials a public key, not a hostname.
	#[error("Invalid URL: host is not an iroh endpoint id")]
	InvalidEndpointId(#[source] iroh::KeyParsingError),

	/// The URL could not be rewritten to the `https` scheme for the H3 request.
	#[error("invalid URL")]
	InvalidUrl,

	/// The rewritten URL failed to parse.
	#[error(transparent)]
	Url(#[from] url::ParseError),

	/// The client connected but never sent a valid WebTransport CONNECT request.
	#[error("failed to receive WebTransport request")]
	RecvRequest(#[source] web_transport_iroh::ServerError),

	/// GSO is always on for iroh, so `--quic-gso=false` cannot be honored.
	#[error("the iroh backend cannot disable GSO; drop --quic-gso=false or use the quinn backend")]
	GsoUnsupported,
}

type Result<T> = std::result::Result<T, Error>;

/// Settings for the shared iroh endpoint, used by both the client and server.
#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct EndpointConfig {
	/// Whether to enable iroh support.
	#[arg(
		id = "iroh-enabled",
		long = "iroh-enabled",
		env = "MOQ_IROH_ENABLED",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub enabled: Option<bool>,

	/// Secret key for the iroh endpoint, either a hex-encoded string or a path to a file.
	/// If the file does not exist, a random key will be generated and written to the path.
	#[arg(id = "iroh-secret", long = "iroh-secret", env = "MOQ_IROH_SECRET")]
	pub secret: Option<String>,

	/// Listen for UDP packets on the given address.
	/// Defaults to `0.0.0.0:0` if not provided.
	#[arg(id = "iroh-bind-v4", long = "iroh-bind-v4", env = "MOQ_IROH_BIND_V4")]
	pub bind_v4: Option<net::SocketAddrV4>,

	/// Listen for UDP packets on the given address.
	/// Defaults to `[::]:0` if not provided.
	#[arg(id = "iroh-bind-v6", long = "iroh-bind-v6", env = "MOQ_IROH_BIND_V6")]
	pub bind_v6: Option<net::SocketAddrV6>,

	/// Disable the iroh relay, using only direct P2P connections.
	#[arg(
		id = "iroh-disable-relay",
		long = "iroh-disable-relay",
		env = "MOQ_IROH_DISABLE_RELAY",
		default_missing_value = "true",
		num_args = 0..=1,
		require_equals = true,
		value_parser = clap::value_parser!(bool),
	)]
	pub disable_relay: Option<bool>,
}

impl EndpointConfig {
	/// Bind the iroh endpoint, applying the per-connection [`crate::quic::Client`] knobs.
	///
	/// iroh is a single P2P endpoint shared by both roles, so it takes the client
	/// section (the per-connection knobs are symmetric). It only honors the knobs
	/// its transport-config builder exposes (stream limits, idle timeout, MTU
	/// discovery); it has no keep-alive knob and cannot disable GSO, so `gso = false`
	/// fails with [`Error::GsoUnsupported`].
	pub async fn bind(self, quic: &crate::quic::Client) -> Result<Option<Endpoint>> {
		if !self.enabled.unwrap_or(false) {
			return Ok(None);
		}

		let quic = quic.resolve();
		if quic.gso_disabled() {
			return Err(Error::GsoUnsupported);
		}

		// If the secret matches the expected format (hex encoded), use it directly.
		let secret_key = if let Some(secret) = self.secret.as_ref().and_then(|s| SecretKey::from_str(s).ok()) {
			secret
		} else if let Some(path) = self.secret {
			let path = PathBuf::from(path);
			if !path.exists() {
				// Generate a new random secret and write it to the file.
				let secret = SecretKey::generate();
				tokio::fs::write(path, hex::encode(secret.to_bytes())).await?;
				secret
			} else {
				// Otherwise, read the secret from a file.
				let key_str = tokio::fs::read_to_string(&path).await?;
				SecretKey::from_str(&key_str).map_err(Error::Secret)?
			}
		} else {
			// Otherwise, generate a new random secret.
			SecretKey::generate()
		};

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = moq_net::ALPNS.iter().map(|alpn| alpn.as_bytes().to_vec()).collect();
		alpns.push(web_transport_iroh::ALPN_H3.as_bytes().to_vec());

		// MoQ opens a stream per group, so raise the low default; also carry the
		// shared idle-timeout / MTU knobs onto iroh's own transport config.
		let max_streams = iroh::endpoint::VarInt::from_u64(quic.max_streams).unwrap_or(iroh::endpoint::VarInt::MAX);
		let mut transport = iroh::endpoint::QuicTransportConfig::builder()
			.max_concurrent_bidi_streams(max_streams)
			.max_concurrent_uni_streams(max_streams)
			.max_idle_timeout(Some(quic.idle_timeout.try_into().expect("idle timeout out of range")));
		if !quic.mtu_discovery {
			transport = transport.mtu_discovery_config(None);
		}

		let mut builder = if self.disable_relay.unwrap_or(false) {
			Endpoint::builder(iroh::endpoint::presets::N0DisableRelay)
		} else {
			Endpoint::builder(iroh::endpoint::presets::N0)
		}
		.secret_key(secret_key)
		.alpns(alpns)
		.transport_config(transport.build());
		if let Some(addr) = self.bind_v4 {
			builder = builder.bind_addr(addr)?;
		}
		if let Some(addr) = self.bind_v6 {
			builder = builder.bind_addr(addr)?;
		}

		let endpoint = builder.bind().await?;
		tracing::info!(endpoint_id = %endpoint.id(), "iroh listening");

		Ok(Some(endpoint))
	}
}

/// Accept an iroh connection, negotiate WebTransport or raw QUIC, and complete the
/// handshake. Returns the established session plus the request URL (raw QUIC carries
/// none). iroh exposes no client-certificate identity, so the identity is always `None`.
pub(crate) async fn accept(
	conn: iroh::endpoint::Incoming,
) -> Result<(
	web_transport_iroh::Session,
	Option<Url>,
	Option<crate::tls::PeerIdentity>,
)> {
	let conn = conn.accept()?.await?;
	let alpn = String::from_utf8(conn.alpn().to_vec())?;
	tracing::Span::current().record("id", conn.stable_id());
	tracing::debug!(remote = %conn.remote_id().fmt_short(), %alpn, "accepted");
	match alpn.as_str() {
		web_transport_iroh::ALPN_H3 => {
			let request = web_transport_iroh::H3Request::accept(conn)
				.await
				.map_err(Error::RecvRequest)?;
			let url = Some(request.url.clone());

			let mut response = ConnectResponse::OK;
			if let Some(protocol) = request.protocols.first() {
				response = response.with_protocol(protocol);
			}
			let session = request.respond(response).await.map_err(Error::Server)?;
			Ok((session, url, None))
		}
		// Raw QUIC carries no request URL; the path rides the SETUP.
		alpn if moq_net::ALPNS.contains(&alpn) => {
			let session = web_transport_iroh::QuicRequest::accept(conn).ok();
			Ok((session, None, None))
		}
		_ => Err(Error::UnsupportedAlpn(alpn)),
	}
}

pub(crate) async fn connect(
	endpoint: &Endpoint,
	url: Url,
	addrs: impl IntoIterator<Item = std::net::SocketAddr>,
) -> Result<web_transport_iroh::Session> {
	let host = url.host().ok_or(Error::MissingHost)?.to_string();
	let endpoint_id: iroh::EndpointId = host.parse().map_err(Error::InvalidEndpointId)?;

	// Build an EndpointAddr with any direct IP addresses provided.
	let mut endpoint_addr = iroh::EndpointAddr::new(endpoint_id);
	for addr in addrs {
		endpoint_addr = endpoint_addr.with_ip_addr(addr);
	}

	// We need to use this API to provide multiple ALPNs.
	// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
	let alpn = moq_net::ALPNS[0].as_bytes();
	let mut additional: Vec<Vec<u8>> = moq_net::ALPNS[1..]
		.iter()
		.map(|alpn| alpn.as_bytes().to_vec())
		.collect();
	additional.push(b"h3".to_vec());
	let opts = iroh::endpoint::ConnectOptions::new().with_additional_alpns(additional);

	let mut connecting = endpoint.connect_with_opts(endpoint_addr, alpn, opts).await?;
	let alpn = connecting.alpn().await?;
	let alpn = String::from_utf8(alpn)?;

	let session = match alpn.as_str() {
		web_transport_iroh::ALPN_H3 => {
			let conn = connecting.await?;
			let url = url_set_scheme(url, "https")?;

			let mut request = ConnectRequest::new(url);
			for alpn in moq_net::ALPNS {
				request = request.with_protocol(alpn.to_string());
			}

			web_transport_iroh::Session::connect_h3(conn, request).await?
		}
		alpn if moq_net::ALPNS.contains(&alpn) => {
			let conn = connecting.await?;
			web_transport_iroh::Session::raw(conn)
		}
		_ => return Err(Error::UnsupportedAlpn(alpn)),
	};

	Ok(session)
}

/// Returns a new URL with a changed scheme.
///
/// [`Url::set_scheme`] returns an error if the scheme change is not valid according to
/// [the URL specification's section on legal scheme state overrides](https://url.spec.whatwg.org/#scheme-state).
///
/// This function allows all scheme changes, as long as the resulting URL is valid.
fn url_set_scheme(url: Url, scheme: &str) -> Result<Url> {
	let url = format!(
		"{}:{}",
		scheme,
		url.to_string().split_once(":").ok_or(Error::InvalidUrl)?.1
	)
	.parse()?;
	Ok(url)
}
