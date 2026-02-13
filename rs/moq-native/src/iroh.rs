use std::{net, path::PathBuf, str::FromStr};

use anyhow::Context;
use url::Url;
use web_transport_iroh::{
	http,
	iroh::{self, SecretKey},
};

pub use iroh::Endpoint as IrohEndpoint;

#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
#[non_exhaustive]
pub struct IrohEndpointConfig {
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

impl IrohEndpointConfig {
	pub async fn bind(self) -> anyhow::Result<Option<IrohEndpoint>> {
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
				let secret = SecretKey::generate(&mut rand::rng());
				tokio::fs::write(path, hex::encode(secret.to_bytes())).await?;
				secret
			} else {
				// Otherwise, read the secret from a file.
				let key_str = tokio::fs::read_to_string(&path).await?;
				SecretKey::from_str(&key_str)?
			}
		} else {
			// Otherwise, generate a new random secret.
			SecretKey::generate(&mut rand::rng())
		};

		let mut alpns = vec![web_transport_iroh::ALPN_H3.as_bytes().to_vec()];
		for alpn in moq_lite::ALPNS {
			alpns.push(alpn.as_bytes().to_vec());
		}

		let mut builder = IrohEndpoint::builder().secret_key(secret_key).alpns(alpns);
		if let Some(addr) = self.bind_v4 {
			builder = builder.bind_addr_v4(addr);
		}
		if let Some(addr) = self.bind_v6 {
			builder = builder.bind_addr_v6(addr);
		}

		let endpoint = builder.bind().await?;
		tracing::info!(endpoint_id = %endpoint.id(), "iroh listening");

		Ok(Some(endpoint))
	}
}

/// URL schemes supported for connecting to iroh endpoints.
pub const IROH_SCHEMES: [&str; 5] = ["iroh", "moql+iroh", "moqt+iroh", "moqt-15+iroh", "h3+iroh"];

/// Returns `true` if `url` has a scheme included in [`IROH_SCHEMES`].
pub fn is_iroh_url(url: &Url) -> bool {
	IROH_SCHEMES.contains(&url.scheme())
}

pub enum IrohRequest {
	Quic {
		connection: iroh::endpoint::Connection,
	},
	WebTransport {
		request: Box<web_transport_iroh::H3Request>,
	},
}

impl IrohRequest {
	pub async fn accept(conn: iroh::endpoint::Incoming) -> anyhow::Result<Self> {
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
				})
			}
			alpn if moq_lite::ALPNS.contains(&alpn) => Ok(Self::Quic { connection: conn }),
			_ => Err(anyhow::anyhow!("unsupported ALPN: {alpn}")),
		}
	}

	/// Accept the session.
	pub async fn ok(self) -> Result<web_transport_iroh::Session, web_transport_iroh::ServerError> {
		match self {
			IrohRequest::Quic { connection, .. } => Ok(web_transport_iroh::Session::raw(connection)),
			IrohRequest::WebTransport { request } => request.ok().await,
		}
	}

	/// Reject the session.
	pub async fn close(self, status: http::StatusCode) -> Result<(), web_transport_iroh::ServerError> {
		match self {
			IrohRequest::Quic { connection, .. } => {
				let _: () = connection.close(status.as_u16().into(), status.as_str().as_bytes());
				Ok(())
			}
			IrohRequest::WebTransport { request, .. } => request.close(status).await,
		}
	}

	pub fn url(&self) -> Option<&Url> {
		match self {
			IrohRequest::Quic { .. } => None,
			IrohRequest::WebTransport { request } => Some(request.url()),
		}
	}
}

pub(crate) async fn connect(endpoint: &IrohEndpoint, url: Url) -> anyhow::Result<web_transport_iroh::Session> {
	let host = url.host().context("Invalid URL: missing host")?.to_string();
	let endpoint_id: iroh::EndpointId = host.parse().context("Invalid URL: host is not an iroh endpoint id")?;

	// We need to use this API to provide multiple ALPNs
	let alpn = b"h3";
	let opts = iroh::endpoint::ConnectOptions::new()
		.with_additional_alpns(moq_lite::ALPNS.iter().map(|alpn| alpn.as_bytes().to_vec()).collect());

	let mut connecting = endpoint.connect_with_opts(endpoint_id, alpn, opts).await?;
	let alpn = connecting.alpn().await?;
	let alpn = String::from_utf8(alpn).context("failed to decode ALPN")?;

	let session = match alpn.as_str() {
		web_transport_iroh::ALPN_H3 => {
			let conn = connecting.await?;
			let url = url_set_scheme(url, "https")?;
			web_transport_iroh::Session::connect_h3(conn, url).await?
		}
		alpn if moq_lite::ALPNS.contains(&alpn) => {
			let conn = connecting.await?;
			// TODO: Add support for ALPNs.
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
