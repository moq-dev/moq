use std::{net, path::PathBuf, str::FromStr};

use anyhow::Context;
use url::Url;
use web_transport_iroh::{
	http,
	iroh::{self, SecretKey},
};
// NOTE: web-transport-iroh should re-export proto like web-transport-quinn does.
use web_transport_proto::{ConnectRequest, ConnectResponse};

pub use iroh::Endpoint;

#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[group(id = "iroh")]
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
}

impl EndpointConfig {
	pub async fn bind(self) -> anyhow::Result<Option<Endpoint>> {
		if !self.enabled.unwrap_or(false) {
			return Ok(None);
		}

		// If the secret matches the expected format (hex encoded), use it directly.
		let secret_key = if let Some(secret) = self.secret.as_ref().and_then(|s| SecretKey::from_str(s).ok()) {
			secret
		} else if let Some(path) = self.secret {
			let path = PathBuf::from(path);
			// Generate a new random secret and attempt to write it atomically.
			// If the file already exists (AlreadyExists), read the existing secret instead.
			// This avoids a TOCTOU race between exists() and create_new().
			let secret = SecretKey::generate(&mut rand::rng());
			let data = hex::encode(secret.to_bytes());
			match write_secret_file(&path, data.as_bytes()).await {
				Ok(()) => secret,
				Err(e)
					if e.downcast_ref::<std::io::Error>()
						.is_some_and(|io| io.kind() == std::io::ErrorKind::AlreadyExists) =>
				{
					let key_str = tokio::fs::read_to_string(&path).await?;
					SecretKey::from_str(&key_str)?
				}
				Err(e) => return Err(e),
			}
		} else {
			// Otherwise, generate a new random secret.
			SecretKey::generate(&mut rand::rng())
		};

		// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
		let mut alpns: Vec<Vec<u8>> = moq_lite::ALPNS.iter().map(|alpn| alpn.as_bytes().to_vec()).collect();
		alpns.push(web_transport_iroh::ALPN_H3.as_bytes().to_vec());

		let mut builder = Endpoint::builder().secret_key(secret_key).alpns(alpns);
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
		alpns: Vec<&'static str>,
	},
	WebTransport {
		request: Box<web_transport_iroh::H3Request>,
		alpns: Vec<&'static str>,
	},
}

impl Request {
	pub async fn accept(conn: iroh::endpoint::Incoming, alpns: Vec<&'static str>) -> anyhow::Result<Self> {
		let conn = conn.accept()?.await?;
		let alpn = String::from_utf8(conn.alpn().to_vec()).context("failed to decode ALPN")?;
		tracing::Span::current().record("id", conn.stable_id());
		tracing::debug!(remote = %conn.remote_id().fmt_short(), %alpn, "accepted");
		match alpn.as_str() {
			web_transport_iroh::ALPN_H3 => {
				let request = web_transport_iroh::H3Request::accept(conn)
					.await
					.context("failed to receive WebTransport request")?;
				Ok(Self::WebTransport {
					request: Box::new(request),
					alpns,
				})
			}
			alpn if alpns.contains(&alpn) => Ok(Self::Quic {
				request: web_transport_iroh::QuicRequest::accept(conn),
				alpns,
			}),
			_ => Err(anyhow::anyhow!("unsupported ALPN: {alpn}")),
		}
	}

	/// Accept the session.
	pub async fn ok(self) -> Result<web_transport_iroh::Session, web_transport_iroh::ServerError> {
		match self {
			Request::Quic { request, .. } => Ok(request.ok()),
			Request::WebTransport { request, alpns } => {
				let mut response = ConnectResponse::OK;
				if let Some(protocol) = request.protocols.iter().find(|p| alpns.contains(&p.as_str())) {
					response = response.with_protocol(protocol);
				}
				request.respond(response).await
			}
		}
	}

	/// Reject the session.
	pub async fn close(self, status: http::StatusCode) -> Result<(), web_transport_iroh::ServerError> {
		match self {
			Request::Quic { request, .. } => {
				request.close(status);
				Ok(())
			}
			Request::WebTransport { request, .. } => request.reject(status).await,
		}
	}

	pub fn url(&self) -> Option<&Url> {
		match self {
			Request::Quic { .. } => None,
			Request::WebTransport { request, .. } => Some(&request.url),
		}
	}
}

pub(crate) async fn connect(
	endpoint: &Endpoint,
	url: Url,
	addrs: impl IntoIterator<Item = std::net::SocketAddr>,
	alpns: &[&str],
) -> anyhow::Result<web_transport_iroh::Session> {
	let host = url.host().context("Invalid URL: missing host")?.to_string();
	let endpoint_id: iroh::EndpointId = host.parse().context("Invalid URL: host is not an iroh endpoint id")?;

	// Build an EndpointAddr with any direct IP addresses provided.
	let mut endpoint_addr = iroh::EndpointAddr::new(endpoint_id);
	for addr in addrs {
		endpoint_addr = endpoint_addr.with_ip_addr(addr);
	}

	// We need to use this API to provide multiple ALPNs.
	// H3 is last because it requires WebTransport framing which not all H3 endpoints support.
	anyhow::ensure!(!alpns.is_empty(), "no ALPNs configured");
	let alpn = alpns[0].as_bytes();
	let mut additional: Vec<Vec<u8>> = alpns[1..].iter().map(|alpn| alpn.as_bytes().to_vec()).collect();
	additional.push(b"h3".to_vec());
	let opts = iroh::endpoint::ConnectOptions::new().with_additional_alpns(additional);

	let mut connecting = endpoint.connect_with_opts(endpoint_addr, alpn, opts).await?;
	let alpn = connecting.alpn().await?;
	let alpn = String::from_utf8(alpn).context("failed to decode ALPN")?;

	let session = match alpn.as_str() {
		web_transport_iroh::ALPN_H3 => {
			let conn = connecting.await?;
			let url = url_set_scheme(url, "https")?;

			let mut request = ConnectRequest::new(url);
			for alpn in alpns {
				request = request.with_protocol(alpn.to_string());
			}

			web_transport_iroh::Session::connect_h3(conn, request).await?
		}
		alpn if alpns.contains(&alpn) => {
			let conn = connecting.await?;
			web_transport_iroh::Session::raw(conn)
		}
		_ => anyhow::bail!("unsupported ALPN: {alpn}"),
	};

	Ok(session)
}

/// Returns a new URL with a changed scheme.
///
/// [`Url::set_scheme`] returns an error if the scheme change is not valid according to
/// [the URL specification's section on legal scheme state overrides](https://url.spec.whatwg.org/#scheme-state).
///
/// This function allows all scheme changes, as long as the resulting URL is valid.
fn url_set_scheme(url: Url, scheme: &str) -> anyhow::Result<Url> {
	let url = format!(
		"{}:{}",
		scheme,
		url.to_string().split_once(":").context("invalid URL")?.1
	)
	.parse()?;
	Ok(url)
}

/// Write secret key data to a file with owner-only permissions (0o600 on Unix).
///
/// Uses `create_new(true)` so the call fails with `AlreadyExists` if the file
/// already exists, which callers can use to handle races atomically.
async fn write_secret_file(path: &std::path::Path, data: &[u8]) -> anyhow::Result<()> {
	use tokio::io::AsyncWriteExt;

	let mut opts = tokio::fs::OpenOptions::new();
	opts.write(true).create_new(true);

	#[cfg(unix)]
	opts.mode(0o600);

	let mut file = opts.open(path).await?;
	file.write_all(data).await.context("failed to write secret key")?;
	Ok(())
}
