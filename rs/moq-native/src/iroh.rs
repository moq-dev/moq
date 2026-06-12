use std::{net, path::PathBuf, str::FromStr};

use url::Url;
use web_transport_iroh::{
	http,
	iroh::{self, SecretKey},
};
// NOTE: web-transport-iroh should re-export proto like web-transport-quinn does.
use web_transport_proto::{ConnectRequest, ConnectResponse};

pub use iroh::Endpoint;
pub use web_transport_iroh;

/// Errors specific to the iroh P2P backend.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error("invalid iroh secret key")]
	Secret(#[source] iroh::KeyParsingError),

	#[error(transparent)]
	Bind(#[from] iroh::endpoint::BindError),

	#[error(transparent)]
	BindAddr(#[from] iroh::endpoint::InvalidSocketAddr),

	#[error(transparent)]
	Connect(#[from] iroh::endpoint::ConnectWithOptsError),

	#[error(transparent)]
	Connecting(#[from] iroh::endpoint::ConnectingError),

	#[error(transparent)]
	Alpn(#[from] iroh::endpoint::AlpnError),

	#[error(transparent)]
	Connection(#[from] iroh::endpoint::ConnectionError),

	#[error(transparent)]
	Client(#[from] web_transport_iroh::ClientError),

	#[error(transparent)]
	Server(#[from] web_transport_iroh::ServerError),

	#[error("failed to decode ALPN")]
	DecodeAlpn(#[from] std::string::FromUtf8Error),

	#[error("unsupported ALPN: {0}")]
	UnsupportedAlpn(String),

	#[error("Invalid URL: missing host")]
	MissingHost,

	#[error("Invalid URL: host is not an iroh endpoint id")]
	InvalidEndpointId(#[source] iroh::KeyParsingError),

	#[error("invalid URL")]
	InvalidUrl,

	#[error(transparent)]
	Url(#[from] url::ParseError),

	#[error("failed to receive WebTransport request")]
	RecvRequest(#[source] web_transport_iroh::ServerError),
}

type Result<T> = std::result::Result<T, Error>;

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
	pub async fn bind(self) -> Result<Option<Endpoint>> {
		if !self.enabled.unwrap_or(false) {
			return Ok(None);
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

		let mut builder = if self.disable_relay.unwrap_or(false) {
			Endpoint::builder(iroh::endpoint::presets::N0DisableRelay)
		} else {
			Endpoint::builder(iroh::endpoint::presets::N0)
		}
		.secret_key(secret_key)
		.alpns(alpns);
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

pub enum Request {
	Quic {
		request: web_transport_iroh::QuicRequest,
	},
	WebTransport {
		request: Box<web_transport_iroh::H3Request>,
	},
}

impl Request {
	pub async fn accept(conn: iroh::endpoint::Incoming) -> Result<Self> {
		let conn = conn.accept()?.await?;
		let alpn = String::from_utf8(conn.alpn().to_vec())?;
		tracing::Span::current().record("id", conn.stable_id());
		tracing::debug!(remote = %conn.remote_id().fmt_short(), %alpn, "accepted");
		match alpn.as_str() {
			web_transport_iroh::ALPN_H3 => {
				let request = web_transport_iroh::H3Request::accept(conn)
					.await
					.map_err(Error::RecvRequest)?;
				Ok(Self::WebTransport {
					request: Box::new(request),
				})
			}
			alpn if moq_net::ALPNS.contains(&alpn) => Ok(Self::Quic {
				request: web_transport_iroh::QuicRequest::accept(conn),
			}),
			_ => Err(Error::UnsupportedAlpn(alpn)),
		}
	}

	/// Accept the session.
	pub async fn ok(self) -> std::result::Result<web_transport_iroh::Session, web_transport_iroh::ServerError> {
		match self {
			Request::Quic { request } => Ok(request.ok()),
			Request::WebTransport { request } => {
				let mut response = ConnectResponse::OK;
				if let Some(protocol) = request.protocols.first() {
					response = response.with_protocol(protocol);
				}
				request.respond(response).await
			}
		}
	}

	/// Reject the session.
	pub async fn close(self, status: http::StatusCode) -> std::result::Result<(), web_transport_iroh::ServerError> {
		match self {
			Request::Quic { request } => {
				request.close(status);
				Ok(())
			}
			Request::WebTransport { request, .. } => request.reject(status).await,
		}
	}

	pub fn url(&self) -> Option<&Url> {
		match self {
			Request::Quic { .. } => None,
			Request::WebTransport { request } => Some(&request.url),
		}
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
