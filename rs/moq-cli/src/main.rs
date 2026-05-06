mod client;
mod publish;
mod server;
mod subscribe;
mod web;

use client::*;
use hang::moq_lite;
use publish::*;
use server::*;
use subscribe::*;
use web::*;

use clap::{Parser, Subcommand};
use std::path::PathBuf;
use url::Url;

#[derive(Parser, Clone)]
#[command(version = env!("VERSION"))]
pub struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	/// Iroh configuration
	#[command(flatten)]
	#[cfg(feature = "iroh")]
	iroh: moq_native::IrohEndpointConfig,

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
	/// Subscribe to a broadcast and write the media to stdout.
	Subscribe {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		#[arg(long)]
		url: Url,

		/// The name of the broadcast to subscribe to.
		#[arg(long)]
		name: String,

		#[command(flatten)]
		args: SubscribeArgs,
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

	#[cfg(feature = "iroh")]
	let iroh = cli.iroh.bind().await?;

	match cli.command {
		Command::Serve {
			config,
			dir,
			name,
			format,
		} => {
			let publish = Publish::new(&format)?;
			let web_bind = config.bind.clone().unwrap_or_else(|| "[::]:443".to_string());

			let server = config.init()?;
			#[cfg(feature = "iroh")]
			let server = server.with_iroh(iroh);

			let web_tls = server.tls_info();

			tokio::select! {
				res = run_server(server, name, publish.consume()) => res,
				res = run_web(&web_bind, web_tls, dir) => res,
				res = publish.run() => res,
			}
		}
		Command::Publish {
			config,
			url,
			name,
			format,
		} => {
			let publish = Publish::new(&format)?;
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			run_client(client, url, name, publish).await
		}
		Command::Subscribe {
			config,
			url,
			name,
			args,
		} => {
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			run_subscribe(client, url, name, args).await
		}
	}
}

async fn run_subscribe(client: moq_native::Client, url: Url, name: String, args: SubscribeArgs) -> anyhow::Result<()> {
	let origin = moq_lite::Origin::random().produce();
	let consumer = origin.consume();

	tracing::info!(%url, %name, "connecting");

	let reconnect = client.with_consume(origin).reconnect(url);

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	let broadcast = consumer
		.announced_broadcast(&name)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast was announced"))?;

	let subscribe = Subscribe::new(broadcast, args);

	tokio::select! {
		res = subscribe.run() => res,
		res = reconnect.closed() => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}
