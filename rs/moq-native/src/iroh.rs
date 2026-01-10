use std::{net, path::PathBuf, str::FromStr};

use url::Url;
use web_transport_iroh::{
	http,
	iroh::{self, SecretKey},
};

pub use iroh::Endpoint;

#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EndpointConfig {
	/// Whether to enable iroh support.
	///
	/// NOTE: The feature flag `iroh` must also be enabled.
	#[arg(
		id = "iroh-enabled",
		long = "iroh-enabled",
		env = "MOQ_IROH_ENABLED",
		default_missing_value = "true",
		num_args = 0,
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

		let mut builder = Endpoint::builder().secret_key(secret_key).alpns(vec![
			web_transport_iroh::ALPN_H3.as_bytes().to_vec(),
			moq_lite::lite::ALPN.as_bytes().to_vec(),
			moq_lite::ietf::ALPN.as_bytes().to_vec(),
		]);
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
pub const IROH_SCHEMES: [&str; 4] = ["iroh", "moql+iroh", "moqt+iroh", "h3+iroh"];

/// Returns `true` if `url` has a scheme included in [`IROH_SCHEMES`].
pub fn is_iroh_url(url: &Url) -> bool {
	IROH_SCHEMES.contains(&url.scheme())
}

/// Raw QUIC-only iroh request (not using HTTP/3).
pub struct IrohQuicRequest(iroh::endpoint::Connection);

impl IrohQuicRequest {
	/// Accept a new QUIC-only WebTransport session from a client.
	pub fn accept(conn: iroh::endpoint::Connection) -> Self {
		Self(conn)
	}

	/// Accept the session.
	pub fn ok(self) -> web_transport_iroh::Session {
		web_transport_iroh::Session::raw(self.0)
	}

	/// Reject the session.
	pub fn close(self, status: http::StatusCode) {
		self.0.close(status.as_u16().into(), status.as_str().as_bytes());
	}
}
