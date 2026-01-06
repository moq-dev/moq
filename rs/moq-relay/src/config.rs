use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::{AuthConfig, ClusterConfig, WebConfig};

#[derive(Parser, Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
	/// The QUIC/TLS configuration for the server.
	#[command(flatten)]
	pub server: moq_native::ServerConfig,

	/// The QUIC/TLS configuration for the client. (clustering only)
	#[command(flatten)]
	#[serde(default)]
	pub client: moq_native::ClientConfig,

	/// Log configuration.
	#[command(flatten)]
	#[serde(default)]
	pub log: moq_native::Log,

	/// Cluster configuration.
	#[command(flatten)]
	#[serde(default)]
	pub cluster: ClusterConfig,

	/// Authentication configuration.
	#[command(flatten)]
	#[serde(default)]
	pub auth: AuthConfig,

	/// Optionally run a TCP HTTP/WebSocket server.
	#[command(flatten)]
	#[serde(default)]
	pub web: WebConfig,

	/// If provided, load the configuration from this file.
	#[serde(default)]
	pub file: Option<String>,
}

impl Config {
	pub fn load() -> anyhow::Result<Self> {
		// Parse just the CLI arguments initially.
		let mut config = Config::parse();

		// If a file is provided, load it and merge the CLI arguments.
		if let Some(file) = config.file {
			config = toml::from_str(&std::fs::read_to_string(file)?)?;
			config.update_from(std::env::args());
		}

		config.log.init();
		tracing::trace!(?config, "final config");

		Ok(config)
	}
}
