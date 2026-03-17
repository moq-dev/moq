mod publish;
mod subscribe;
mod web;

use publish::*;
use subscribe::*;
use web::*;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use url::Url;

#[derive(Parser, Clone)]
pub struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	#[cfg(feature = "iroh")]
	#[command(flatten)]
	iroh: moq_native::IrohEndpointConfig,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
pub enum Command {
	/// Run as a server (listen for connections)
	Server {
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// Optionally serve static files from the given directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		#[command(subcommand)]
		action: ServerAction,
	},
	/// Run as a client (connect to a server)
	Client {
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		#[arg(long)]
		url: Url,

		#[command(subcommand)]
		action: ClientAction,
	},
}

#[derive(Subcommand, Clone)]
pub enum ServerAction {
	/// Publish media from stdin to connecting clients
	Publish {
		#[arg(long)]
		name: String,

		#[command(flatten)]
		args: PublishArgs,
	},
	/// Subscribe to media from a connecting client, write to stdout
	Subscribe {
		#[arg(long)]
		name: String,

		#[command(flatten)]
		args: SubscribeArgs,
	},
}

#[derive(Subcommand, Clone)]
pub enum ClientAction {
	/// Publish media from stdin to the server
	Publish {
		#[arg(long)]
		name: String,

		#[command(flatten)]
		args: PublishArgs,
	},
	/// Subscribe to media from the server, write to stdout
	Subscribe {
		#[arg(long)]
		name: String,

		#[command(flatten)]
		args: SubscribeArgs,
	},
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let cli = Cli::parse();
	cli.log.init()?;

	#[cfg(feature = "iroh")]
	let iroh = cli.iroh.bind().await?;

	match cli.command {
		Command::Server { config, dir, action } => {
			let web_bind = config.bind.unwrap_or("[::]:443".parse().unwrap());

			let server = config.init()?;
			#[cfg(feature = "iroh")]
			let server = server.with_iroh(iroh);

			let web_tls = server.tls_info();

			match action {
				ServerAction::Publish { name, args } => {
					let publish = Publish::new(&args)?;

					tokio::select! {
						res = publish.run_server(server, name) => res,
						res = run_web(web_bind, web_tls, dir) => res,
					}
				}
				ServerAction::Subscribe { name, args } => {
					tokio::select! {
						res = Subscribe::run_server(server, name, args) => res,
						res = run_web(web_bind, web_tls, dir) => res,
					}
				}
			}
		}
		Command::Client { config, url, action } => {
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			match action {
				ClientAction::Publish { name, args } => {
					let publish = Publish::new(&args)?;
					publish.run_client(client, url, name).await
				}
				ClientAction::Subscribe { name, args } => Subscribe::run_client(client, url, name, args).await,
			}
		}
	}
}
