//! `moq-hls` binary.
//!
//! Two subcommands under shared relay/client globals:
//!
//! - `serve` -- subscribe to MoQ broadcasts and serve HLS + LL-HLS over HTTP
//!   (an HTTP *server* that *subscribes*; the WHEP-server analogue in `moq-rtc`).
//! - `ingest` -- pull a remote HLS playlist and publish it into MoQ (an HTTP
//!   *client* that *publishes*; the WHEP-client analogue in `moq-rtc`).
//!
//! HLS isn't a symmetric push/pull protocol like WHIP/WHEP, so these are
//! explicit subcommands rather than a `server`/`client` x `publish`/`subscribe`
//! matrix.

use std::net::SocketAddr;
use std::path::PathBuf;
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

	/// URL of the upstream MoQ relay to publish into (ingest) or read from (serve).
	#[arg(long, env = "MOQ_HLS_RELAY")]
	relay: Url,

	#[command(subcommand)]
	command: Command,
}

#[derive(Subcommand, Clone)]
enum Command {
	/// Serve HLS / LL-HLS over HTTP from MoQ broadcasts (path-based, multi-broadcast).
	Serve {
		/// HTTP listener for the HLS endpoints.
		#[arg(long, env = "MOQ_HLS_LISTEN", default_value = "[::]:8089")]
		listen: SocketAddr,

		/// Optional TLS cert (PEM). Requires `--tls-key`.
		#[arg(long, env = "MOQ_HLS_TLS_CERT", requires = "tls_key")]
		tls_cert: Option<PathBuf>,

		#[arg(long, env = "MOQ_HLS_TLS_KEY", requires = "tls_cert")]
		tls_key: Option<PathBuf>,

		/// LL-HLS part target duration, in milliseconds.
		#[arg(long, env = "MOQ_HLS_PART_TARGET_MS", default_value = "500")]
		part_target_ms: u64,

		/// Number of segments kept in each rendition's sliding window.
		#[arg(long, env = "MOQ_HLS_WINDOW", default_value = "8")]
		window: usize,
	},
	/// Pull a remote HLS master/media playlist and publish it into MoQ.
	Ingest {
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
		Command::Serve {
			listen,
			tls_cert,
			tls_key,
			part_target_ms,
			window,
		} => {
			let subscriber = moq_net::Origin::random().produce();
			let subscriber_consumer = subscriber.consume();
			let reconnect = client.with_consumer(subscriber).reconnect(relay.clone());

			let config = moq_hls::egress::Config {
				part_target: Duration::from_millis(part_target_ms),
				window,
				..Default::default()
			};
			let server = Server::new(subscriber_consumer, config);
			let app = server
				.router()
				.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));

			tracing::info!(%relay, %listen, "moq-hls serving HLS");

			#[cfg(unix)]
			let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

			tokio::select! {
				res = serve(app, listen, tls_cert, tls_key) => res,
				res = reconnect.closed() => res,
				_ = tokio::signal::ctrl_c() => Ok(()),
			}
		}
		Command::Ingest { broadcast, playlist } => {
			let publisher = moq_net::Origin::random().produce();
			let reconnect = client.with_publisher(publisher.clone()).reconnect(relay.clone());

			let mut producer = moq_net::BroadcastInfo::new().produce();
			let consumer = producer.consume();
			let _publish = publisher
				.publish_broadcast(&broadcast, consumer)
				.context("failed to publish broadcast")?;

			let catalog = moq_mux::catalog::hang::Producer::new(&mut producer).context("failed to create catalog")?;
			let mut importer = moq_hls::ingest::Import::new(producer, catalog, moq_hls::ingest::Config::new(playlist))?;

			tracing::info!(%relay, %broadcast, "moq-hls ingesting HLS");

			#[cfg(unix)]
			let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

			tokio::select! {
				res = async {
					importer.init().await?;
					importer.run().await
				} => res.map_err(Into::into),
				res = reconnect.closed() => res,
				_ = tokio::signal::ctrl_c() => Ok(()),
			}
		}
	}
}

async fn serve(app: Router, bind: SocketAddr, cert: Option<PathBuf>, key: Option<PathBuf>) -> anyhow::Result<()> {
	let service = app.into_make_service();
	match (cert, key) {
		(Some(cert), Some(key)) => {
			let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key)
				.await
				.context("failed to load TLS cert/key")?;
			axum_server::bind_rustls(bind, config).serve(service).await?;
		}
		(None, None) => {
			axum_server::bind(bind).serve(service).await?;
		}
		// clap's `requires` already gates this; explicit arm in case the attr is stripped.
		(Some(_), None) | (None, Some(_)) => anyhow::bail!("--tls-cert and --tls-key must be set together"),
	}
	Ok(())
}
