mod client;
mod publish;
mod server;
mod subscribe;
mod web;

use client::*;
use hang::moq_net;
use publish::*;
use server::*;
use subscribe::*;
use web::*;

use clap::{Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tower_http::cors::{Any, CorsLayer};
use url::Url;

#[derive(Parser, Clone)]
#[command(version = env!("VERSION"))]
pub struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	/// Iroh configuration
	#[command(flatten)]
	#[cfg(feature = "iroh")]
	iroh: moq_native::iroh::EndpointConfig,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
pub enum Command {
	/// Run a relay and publish a single broadcast read from stdin into it.
	Serve {
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// The name of the broadcast to serve.
		#[arg(long, alias = "name")]
		broadcast: String,

		/// Optionally serve static files from the given directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		/// The format of the input media.
		#[command(subcommand)]
		format: PublishFormat,
	},
	/// Run a relay and write the first incoming broadcast's media to stdout.
	Accept {
		#[command(flatten)]
		config: moq_native::ServerConfig,

		/// The name of the broadcast to accept.
		#[arg(long, alias = "name")]
		broadcast: String,

		/// Optionally serve static files from the given directory.
		#[arg(long)]
		dir: Option<PathBuf>,

		#[command(flatten)]
		args: SubscribeArgs,
	},
	/// Publish a broadcast read from stdin to a remote relay.
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
		#[arg(long, alias = "name")]
		broadcast: String,

		/// The format of the input media.
		#[command(subcommand)]
		format: PublishFormat,
	},
	/// Subscribe to a broadcast on a remote relay and write the media to stdout.
	Subscribe {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		#[arg(long)]
		url: Url,

		/// The name of the broadcast to subscribe to.
		#[arg(long, alias = "name")]
		broadcast: String,

		#[command(flatten)]
		args: SubscribeArgs,
	},
	/// Import or export HLS / LL-HLS via a MoQ relay.
	Hls {
		/// The MoQ client configuration.
		#[command(flatten)]
		config: moq_native::ClientConfig,

		/// The URL of the MoQ server.
		#[arg(long, alias = "relay", env = "MOQ_HLS_RELAY")]
		url: Url,

		#[command(subcommand)]
		command: HlsCommand,
	},
}

#[derive(Subcommand, Clone)]
pub enum HlsCommand {
	/// Serve HLS / LL-HLS over HTTP from MoQ broadcasts.
	Export {
		/// HTTP listener for the HLS endpoints.
		#[arg(long, env = "MOQ_HLS_LISTEN", default_value = "[::]:8089")]
		listen: SocketAddr,

		/// TLS certificates, keys, self-signed generation, and optional mTLS roots.
		#[command(flatten)]
		tls: moq_native::tls::Server,

		/// LL-HLS part target duration.
		#[arg(long, env = "MOQ_HLS_PART_TARGET", default_value = "500ms", value_parser = humantime::parse_duration)]
		part_target: Duration,

		/// Minimum duration of media kept in each rendition's sliding window.
		#[arg(long, env = "MOQ_HLS_WINDOW", default_value = "16s", value_parser = humantime::parse_duration)]
		window: Duration,
	},
	/// Pull a remote HLS master/media playlist and publish it into MoQ.
	Import {
		/// Broadcast name to publish on the relay.
		#[arg(long, alias = "name", env = "MOQ_HLS_BROADCAST")]
		broadcast: String,

		/// Remote HLS playlist URL (http/https) or local file path.
		#[arg(long, env = "MOQ_HLS_PLAYLIST")]
		playlist: String,
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
	cli.log.init()?;

	#[cfg(feature = "iroh")]
	let iroh = cli.iroh.bind().await?;

	match cli.command {
		Command::Serve {
			config,
			dir,
			broadcast,
			format,
		} => {
			warn_if_missing_format(&broadcast);
			let publish = Publish::new(&format)?;
			let web_bind = config.bind.clone().unwrap_or_else(|| "[::]:443".to_string());

			let server = config.init()?;
			#[cfg(feature = "iroh")]
			let server = server.with_iroh(iroh);

			let web_tls = server.tls_info();

			tokio::select! {
				res = run_server(server, broadcast, publish.consume()) => res,
				res = run_web(&web_bind, web_tls, dir) => res,
				res = publish.run() => res,
			}
		}
		Command::Accept {
			config,
			broadcast,
			dir,
			args,
		} => {
			let web_bind = config.bind.clone().unwrap_or_else(|| "[::]:443".to_string());

			let server = config.init()?;
			#[cfg(feature = "iroh")]
			let server = server.with_iroh(iroh);

			let web_tls = server.tls_info();

			let origin = moq_net::Origin::random().produce();
			let consumer = origin.consume();

			tokio::select! {
				res = run_accept(server, origin) => res,
				res = run_web(&web_bind, web_tls, dir) => res,
				res = run_announced_subscribe(consumer, broadcast, args) => res,
				_ = tokio::signal::ctrl_c() => Ok(()),
			}
		}
		Command::Publish {
			config,
			url,
			broadcast,
			format,
		} => {
			warn_if_missing_format(&broadcast);
			let publish = Publish::new(&format)?;
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			run_client(client, url, broadcast, publish).await
		}
		Command::Subscribe {
			config,
			url,
			broadcast,
			args,
		} => {
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			run_subscribe(client, url, broadcast, args).await
		}
		Command::Hls { config, url, command } => {
			let client = config.init()?;

			#[cfg(feature = "iroh")]
			let client = client.with_iroh(iroh);

			match command {
				HlsCommand::Export {
					listen,
					tls,
					part_target,
					window,
				} => run_hls_export(client, url, listen, tls, part_target, window).await,
				HlsCommand::Import { broadcast, playlist } => run_hls_import(client, url, broadcast, playlist).await,
			}
		}
	}
}

fn warn_if_missing_format(name: &str) {
	if moq_mux::catalog::CatalogFormat::detect(name).is_none() {
		tracing::warn!(
			name,
			"You should append .hang to your broadcast name to make the catalog format explicit."
		);
	}
}

async fn run_subscribe(
	client: moq_native::Client,
	url: Url,
	broadcast: String,
	args: SubscribeArgs,
) -> anyhow::Result<()> {
	let origin = moq_net::Origin::random().produce();
	let consumer = origin.consume();

	tracing::info!(%url, %broadcast, "connecting");

	let reconnect = client.with_subscriber(origin).reconnect(url);

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = run_announced_subscribe(consumer, broadcast, args) => res,
		res = reconnect.closed() => Ok(res?),
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}

async fn run_announced_subscribe(
	consumer: moq_net::OriginConsumer,
	broadcast: String,
	args: SubscribeArgs,
) -> anyhow::Result<()> {
	let catalog = args.catalog_format(&broadcast);

	let consumer = consumer
		.announced_broadcast(&broadcast)
		.await
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast was announced"))?;

	Subscribe::new(consumer, catalog, args).run().await
}

async fn run_hls_export(
	client: moq_native::Client,
	url: Url,
	listen: SocketAddr,
	tls: moq_native::tls::Server,
	part_target: Duration,
	window: Duration,
) -> anyhow::Result<()> {
	let subscriber = moq_net::Origin::random().produce();
	let consumer = subscriber.consume();
	let reconnect = client.with_subscriber(subscriber).reconnect(url.clone());

	let config = moq_hls::export::Config {
		part_target,
		window,
		..Default::default()
	};
	let server = moq_hls::Server::new(consumer, config);
	let app = server
		.router()
		.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));

	let tls = if tls.cert.is_empty() && tls.generate.is_empty() {
		None
	} else {
		let alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
		Some(tls.server_config(alpn)?)
	};

	let listener = moq_native::bind::tcp(listen)?;

	tracing::info!(%url, %listen, "serving HLS");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	tokio::select! {
		res = serve_hls(listener, app, tls) => res,
		res = reconnect.closed() => res.map_err(Into::into),
		_ = shutdown_signal() => Ok(()),
	}
}

async fn run_hls_import(
	client: moq_native::Client,
	url: Url,
	broadcast: String,
	playlist: String,
) -> anyhow::Result<()> {
	warn_if_missing_format(&broadcast);

	let publisher = moq_net::Origin::random().produce();
	let reconnect = client.with_publisher(publisher.consume()).reconnect(url.clone());

	let mut producer = moq_net::BroadcastInfo::new().produce();
	let consumer = producer.consume();
	// Held for the import's lifetime; dropping the guard unannounces the broadcast.
	let _publish = publisher.publish_broadcast(&broadcast, consumer)?;

	let catalog = moq_mux::catalog::Producer::new(&mut producer)?;
	let mut importer = moq_hls::import::Import::new(producer, catalog, moq_hls::import::Config::new(playlist))?;

	tracing::info!(%url, %broadcast, "importing HLS");

	tokio::select! {
		res = async {
			importer.init().await?;

			#[cfg(unix)]
			let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

			importer.run().await
		} => res.map_err(Into::into),
		res = reconnect.closed() => res.map_err(Into::into),
		_ = shutdown_signal() => Ok(()),
	}
}

async fn serve_hls(
	listener: std::net::TcpListener,
	app: axum::Router,
	tls: Option<Arc<rustls::ServerConfig>>,
) -> anyhow::Result<()> {
	let service = app.into_make_service();
	match tls {
		Some(config) => {
			let config = axum_server::tls_rustls::RustlsConfig::from_config(config);
			axum_server::from_tcp_rustls(listener, config)?.serve(service).await?;
		}
		None => {
			axum_server::from_tcp(listener)?.serve(service).await?;
		}
	}
	Ok(())
}

async fn shutdown_signal() {
	#[cfg(unix)]
	{
		use tokio::signal::unix::{SignalKind, signal};

		let mut term = match signal(SignalKind::terminate()) {
			Ok(term) => term,
			Err(_) => {
				let _ = tokio::signal::ctrl_c().await;
				return;
			}
		};
		tokio::select! {
			_ = tokio::signal::ctrl_c() => {}
			_ = term.recv() => {}
		}
	}

	#[cfg(not(unix))]
	{
		let _ = tokio::signal::ctrl_c().await;
	}
}
