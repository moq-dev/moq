use std::{net, path::PathBuf, str::FromStr};

use web_transport_iroh::iroh::{Endpoint, SecretKey};

mod client;
mod server;

pub use self::client::*;
pub use self::server::*;

#[derive(clap::Args, Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EndpointConfig {
	/// Path to a secret key for the iroh endpoint.
	///
	/// If the file doesn't exist, a random key will be generated and stored there.
	#[arg(
		id = "iroh-secret-key-path",
		long = "iroh-secret-key-path",
		env = "MOQ_IROH_SECRET_PATH",
		conflicts_with = "iroh-secret-key"
	)]
	secret_key_path: Option<PathBuf>,
	/// Secret key for the iroh endpoint.
	#[arg(
		id = "iroh-secret-key",
		long = "iroh-secret-key",
		env = "MOQ_IROH_SECRET",
		conflicts_with = "iroh-secret-key-path"
	)]
	secret_key: Option<String>,
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
	pub async fn bind(self) -> anyhow::Result<Endpoint> {
		let secret_key = match (self.secret_key, self.secret_key_path) {
			(Some(key), None) => SecretKey::from_str(&key)?,
			(None, Some(path)) => {
				if path.exists() {
					let key_str = tokio::fs::read_to_string(&path).await?;
					SecretKey::from_str(&key_str)?
				} else {
					let key = SecretKey::generate(&mut rand::rng());
					let key_str = hex::encode(key.to_bytes());
					tokio::fs::write(path, key_str).await?;
					key
				}
			}
			(None, None) => SecretKey::generate(&mut rand::rng()),
			(Some(_), Some(_)) => anyhow::bail!("Setting both secret_key and secret_key_path is invalid"),
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
		Ok(endpoint)
	}

	pub async fn init_server(self) -> anyhow::Result<Server> {
		Server::new(self).await
	}

	pub async fn init_client(self) -> anyhow::Result<Client> {
		Client::new(self).await
	}
}
