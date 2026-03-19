mod publish;
mod subscribe;
mod web;

use publish::*;
use subscribe::*;
use web::*;

use clap::{Parser, Subcommand};
use hang::moq_lite;
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
		action: Action,
	},
	/// Run as a client (connect to a server)
	Client {
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		#[arg(long)]
		url: Url,

		#[command(subcommand)]
		action: Action,
	},
}

#[derive(Subcommand, Clone)]
pub enum Action {
	/// Publish media from stdin
	Publish {
		#[arg(long)]
		name: String,

		#[command(flatten)]
		args: PublishArgs,
	},
	/// Subscribe to media, write to stdout
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
				Action::Publish { name, args } => {
					let publish = Publish::new(&args)?;
					let broadcast = publish.consume();

					tokio::select! {
						res = run_publish_server(server, name, broadcast) => res,
						res = publish.run() => res,
						res = run_web(web_bind, web_tls, dir) => res,
					}
				}
				Action::Subscribe { name, args } => {
					let origin = moq_lite::Origin::produce();
					let mut consumer = origin.consume();
					let server = server.with_consume(origin);

					tokio::select! {
						res = run_accept(server) => res,
						res = async {
							let broadcast = wait_broadcast(&mut consumer, &name).await?;
							Subscribe::new(broadcast, args).run().await
						} => res,
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
				Action::Publish { name, args } => {
					let publish = Publish::new(&args)?;
					let origin = moq_lite::Origin::produce();
					origin.publish_broadcast(&name, publish.consume());

					tracing::info!(%url, %name, "connecting");
					let session = client.with_publish(origin.consume()).connect(url).await?;

					run_client(session, publish.run()).await
				}
				Action::Subscribe { name, args } => {
					let origin = moq_lite::Origin::produce();
					let mut consumer = origin.consume();

					tracing::info!(%url, %name, "connecting");
					let session = client.with_consume(origin).connect(url).await?;

					let broadcast = wait_broadcast(&mut consumer, &name).await?;
					let subscribe = Subscribe::new(broadcast, args);

					run_client(session, subscribe.run()).await
				}
			}
		}
	}
}

/// Run a client session, waiting for the action to complete, the session to close, or ctrl_c.
async fn run_client(
	mut session: moq_lite::Session,
	action: impl std::future::Future<Output = anyhow::Result<()>>,
) -> anyhow::Result<()> {
	#[cfg(unix)]
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = action => res,
		res = session.closed() => res.map_err(Into::into),
		_ = tokio::signal::ctrl_c() => {
			session.close(moq_lite::Error::Cancel);
			tokio::time::sleep(std::time::Duration::from_millis(100)).await;
			Ok(())
		},
	}
}

/// Wait for a named broadcast to be announced on the origin.
async fn wait_broadcast(
	consumer: &mut moq_lite::OriginConsumer,
	name: &str,
) -> anyhow::Result<moq_lite::BroadcastConsumer> {
	loop {
		let (path, announced) = consumer
			.announced()
			.await
			.ok_or_else(|| anyhow::anyhow!("origin closed"))?;

		if let Some(broadcast) = announced {
			if path.as_ref() == name {
				return Ok(broadcast);
			}
		}
	}
}

/// Accept connections in a loop, publishing the same broadcast to each.
async fn run_publish_server(
	mut server: moq_native::Server,
	name: String,
	broadcast: moq_lite::BroadcastConsumer,
) -> anyhow::Result<()> {
	#[cfg(unix)]
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	let mut conn_id: u64 = 0;

	tracing::info!(addr = ?server.local_addr(), "listening");

	while let Some(request) = server.accept().await {
		let id = conn_id;
		conn_id += 1;

		let name = name.clone();
		let broadcast = broadcast.clone();

		tokio::spawn(async move {
			let origin = moq_lite::Origin::produce();
			origin.publish_broadcast(&name, broadcast);

			match request.with_publish(origin.consume()).ok().await {
				Ok(session) => {
					tracing::info!(id, "accepted session");
					if let Err(err) = session.closed().await {
						tracing::warn!(id, %err, "session error");
					}
				}
				Err(err) => tracing::warn!(id, %err, "failed to accept session"),
			}
		});
	}

	Ok(())
}

/// Accept connections in a loop (origin already configured on the server).
async fn run_accept(mut server: moq_native::Server) -> anyhow::Result<()> {
	#[cfg(unix)]
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	let mut conn_id: u64 = 0;

	tracing::info!(addr = ?server.local_addr(), "listening");

	while let Some(request) = server.accept().await {
		let id = conn_id;
		conn_id += 1;

		tokio::spawn(async move {
			match request.ok().await {
				Ok(session) => {
					tracing::info!(id, "accepted session");
					if let Err(err) = session.closed().await {
						tracing::warn!(id, %err, "session error");
					}
				}
				Err(err) => tracing::warn!(id, %err, "failed to accept session"),
			}
		});
	}

	Ok(())
}
