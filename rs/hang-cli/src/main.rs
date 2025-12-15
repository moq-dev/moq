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
	let cli = Cli::parse();
	cli.log.init();

	let mut publish = Publish::new(match &cli.command {
		Command::Serve { format, .. } => format,
		Command::Publish { format, .. } => format,
	});

	// Initialize the broadcast from stdin before starting any client/server.
	publish.init().await?;

	match cli.command {
		Command::Serve { config, dir, name, .. } => server(config, name, dir, publish).await,
		Command::Publish { config, url, name, .. } => client(config, url, name, publish).await,
	}
}
