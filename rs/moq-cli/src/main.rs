mod client;
mod publish;
mod server;
mod subscribe;
mod web;

use client::*;
use publish::*;
use server::*;
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
						res = run_server(server, name, publish.consume()) => res,
						res = run_web(web_bind, web_tls, dir) => res,
						res = publish.run() => res,
					}
				}
				ServerAction::Subscribe { name, args } => {
					let origin = hang::moq_lite::Origin::produce();
					let mut consumer = origin.consume();

					let session_server = server.with_consume(origin);

					// Run the server in the background, waiting for a broadcast
					tokio::select! {
						res = run_server_consume(session_server, name.clone()) => res,
						res = run_web(web_bind, web_tls, dir) => res,
						res = async {
							// Wait for the named broadcast to be announced
							let broadcast = loop {
								let (path, announced) = consumer
									.announced()
									.await
									.ok_or_else(|| anyhow::anyhow!("origin closed"))?;

								if let Some(broadcast) = announced {
									if path.as_ref() == name {
										break broadcast;
									}
								}
							};

							let subscribe = Subscribe::new(broadcast, args);
							subscribe.run().await
						} => res,
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
					run_client(client, url, name, publish).await
				}
				ClientAction::Subscribe { name, args } => run_client_subscribe(client, url, name, args).await,
			}
		}
	}
}
