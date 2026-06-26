//! `moq-hls` binary.
//!
//! Two subcommands under shared relay/client globals:
//!
//! - `export` -- subscribe to MoQ broadcasts and serve HLS + LL-HLS over HTTP
//!   (an HTTP *server* that *subscribes*; the WHEP-server analogue in `moq-rtc`).
//! - `import` -- pull a remote HLS playlist and publish it into MoQ (an HTTP
//!   *client* that *publishes*; the WHEP-client analogue in `moq-rtc`).
//!
//! HLS isn't a symmetric push/pull protocol like WHIP/WHEP, so these are
//! explicit subcommands rather than a `server`/`client` x `publish`/`subscribe`
//! matrix.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use axum::Router;
use clap::{Parser, Subcommand};
use moq_hls::Server;
use tower_http::cors::{Any, CorsLayer};
use url::Url;

#[derive(Parser, Clone)]
#[command(version)]
struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	/// MoQ client configuration for dialing the upstream relay.
	#[command(flatten)]
	moq_client: moq_native::ClientConfig,

	/// URL of the upstream MoQ relay to publish into (import) or read from (export).
	#[arg(long, env = "MOQ_HLS_RELAY")]
	relay: Url,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
enum Command {
	/// Serve HLS / LL-HLS over HTTP from MoQ broadcasts (path-based, multi-broadcast).
	Export {
		/// HTTP listener for the HLS endpoints.
		#[arg(long, env = "MOQ_HLS_LISTEN", default_value = "[::]:8089")]
		listen: SocketAddr,

		/// TLS certificates, keys, self-signed generation, and optional mTLS roots.
		/// Serve HTTPS by setting `--tls-cert`/`--tls-key` or `--tls-generate`.
		/// Most players require HTTPS.
		#[command(flatten)]
		tls: moq_native::tls::Server,

		/// LL-HLS part target duration (also caps the exporter's fragment duration).
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
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let Cli {
		log,
		moq_client,
		relay,
		command,
	} = Cli::parse();
	log.init()?;

	let client = moq_client.init().context("failed to init moq client")?;

	match command {
		Command::Export {
			listen,
			tls,
			part_target,
			window,
		} => {
			let subscriber = moq_net::Origin::random().produce();
			let subscriber_consumer = subscriber.consume();
			let reconnect = client.with_consume(subscriber).reconnect(relay.clone());

			let config = moq_hls::export::Config {
				part_target,
				window,
				..Default::default()
			};
			let server = Server::new(subscriber_consumer, config);
			let app = server
				.router()
				.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));

			// Serve HTTPS only when a cert/key pair or self-signed generation is configured.
			let tls = if tls.cert.is_empty() && tls.generate.is_empty() {
				None
			} else {
				let alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
				Some(tls.server_config(alpn).context("failed to build TLS config")?)
			};

			// Bind before signaling readiness so a port conflict surfaces as a startup
			// failure instead of systemd briefly seeing a dead instance as healthy.
			let listener = std::net::TcpListener::bind(listen).context("failed to bind HLS listener")?;

			tracing::info!(%relay, %listen, "moq-hls serving HLS");

			#[cfg(unix)]
			let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

			tokio::select! {
				res = serve(listener, app, tls) => res,
				res = reconnect.closed() => res.map_err(Into::into),
				_ = shutdown_signal() => Ok(()),
			}
		}
		Command::Import { broadcast, playlist } => {
			let publisher = moq_net::Origin::random().produce();
			let reconnect = client.with_publish(publisher.consume()).reconnect(relay.clone());

			let mut producer = moq_net::Broadcast::new().produce();
			let consumer = producer.consume();
			anyhow::ensure!(
				publisher.publish_broadcast(&broadcast, consumer),
				"failed to publish broadcast"
			);

			let catalog = moq_mux::catalog::Producer::new(&mut producer).context("failed to create catalog")?;
			let mut importer = moq_hls::import::Import::new(producer, catalog, moq_hls::import::Config::new(playlist))?;

			tracing::info!(%relay, %broadcast, "moq-hls importing HLS");

			tokio::select! {
				res = async {
					// Signal readiness only once the source is validated and primed, not
					// before, so a bad playlist URL fails startup instead of reporting healthy.
					importer.init().await?;

					#[cfg(unix)]
					let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

					importer.run().await
				} => res.map_err(Into::into),
				res = reconnect.closed() => res.map_err(Into::into),
				_ = shutdown_signal() => Ok(()),
			}
		}
	}
}

/// Resolve when the process is asked to shut down: Ctrl-C, or SIGTERM on Unix
/// (which is how systemd and most supervisors stop a service).
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

async fn serve(
	listener: std::net::TcpListener,
	app: Router,
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
