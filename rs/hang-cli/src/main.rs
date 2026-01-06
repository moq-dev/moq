mod client;
mod publish;
mod server;

use client::*;
use publish::*;
use server::*;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use url::Url;

#[derive(Parser, Clone)]
pub struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
pub enum Command {
	Serve {
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// Optionally enable serving via iroh.
		#[cfg(feature = "iroh")]
		#[clap(long)]
		iroh: bool,

		/// Configuration for the iroh endpoint.
		///
		/// Serving over iroh only is enabled effect when `--iroh` is set.
		#[cfg(feature = "iroh")]
		#[command(flatten)]
		iroh_config: moq_native::iroh::EndpointConfig,

		/// The name of the broadcast to serve.
		#[arg(long)]
		name: String,

		/// Optionally serve static files from the given directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		/// The format of the input media.
		#[command(subcommand)]
		format: PublishFormat,
	},
	Publish {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// Configuration for the iroh endpoint.
		///
		/// The settings are optional, the iroh endpoint will always be initiated.
		/// It will be used for URLs with the schemes
		///   iroh://<endpoint-id>
		///   moql+iroh://<endpoint-id>
		///   moqt+iroh://<endpoint-id>
		///   h3+iroh://<endpoint-id>/<optional-path>?<optional-query>
		#[cfg(feature = "iroh")]
		#[command(flatten)]
		iroh_config: moq_native::iroh::EndpointConfig,

		/// The URL of the MoQ server.
		///
		/// The URL must start with `https://` or `http://`.
		/// - If `http` is used, a HTTP fetch to "/certificate.sha256" is first made to get the TLS certificiate fingerprint (insecure).
		/// - If `https` is used, then A WebTransport connection is made via QUIC to the provided host/port.
		///
		/// The `?jwt=` query parameter is used to provide a JWT token from moq-token-cli.
		/// Otherwise, the public path (if any) is used instead.
		///
		/// The path currently must be `/` or you'll get an error on connect.
		#[arg(long)]
		url: Url,

		/// The name of the broadcast to publish.
		#[arg(long)]
		name: String,

		/// The format of the input media.
		#[command(subcommand)]
		format: PublishFormat,
	},
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let cli = Cli::parse();
	cli.log.init();

	let mut publish = Publish::new(match &cli.command {
		Command::Serve { format, .. } => format,
		Command::Publish { format, .. } => format,
	})?;

	// Initialize the broadcast from stdin before starting any client/server.
	publish.init().await?;

	match cli.command {
		Command::Serve {
			config,
			dir,
			name,
			#[cfg(feature = "iroh")]
			iroh,
			#[cfg(feature = "iroh")]
			iroh_config,
			..
		} => {
			#[cfg(feature = "iroh")]
			let config = config.with_iroh(iroh.then_some(iroh_config));
			server(config, name, dir, publish).await
		}
		Command::Publish {
			config,
			#[cfg(feature = "iroh")]
			iroh_config,
			url,
			name,
			..
		} => {
			#[cfg(feature = "iroh")]
			let config = config.with_iroh(Some(iroh_config));
			client(config, url, name, publish).await
		}
	}
}
