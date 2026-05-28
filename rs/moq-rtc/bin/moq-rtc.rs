//! `moq-rtc` binary: WebRTC <-> MoQ gateway.
//!
//! Binds an axum listener serving WHIP/WHEP and dials an upstream moq-net
//! relay where ingested broadcasts are forwarded and egressed broadcasts
//! are pulled from.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::Router;
use clap::Parser;
use moq_rtc::{Gateway, GatewayConfig};
use tower_http::cors::{Any, CorsLayer};
use url::Url;

#[derive(Parser, Clone)]
#[command(version)]
struct Cli {
	#[command(flatten)]
	log: moq_native::Log,

	/// MoQ client configuration for dialing the upstream relay.
	#[command(flatten)]
	client: moq_native::ClientConfig,

	/// URL of the upstream MoQ relay.
	#[arg(long, env = "MOQ_RTC_RELAY")]
	relay: Url,

	/// HTTP listener for the WHIP/WHEP endpoints.
	#[arg(long, env = "MOQ_RTC_LISTEN", default_value = "[::]:8088")]
	listen: SocketAddr,

	/// Optional TLS certificate (PEM) for serving HTTPS instead of HTTP.
	/// Requires `--tls-key`. WHIP clients typically need HTTPS in practice.
	#[arg(long, env = "MOQ_RTC_TLS_CERT", requires = "tls_key")]
	tls_cert: Option<PathBuf>,

	#[arg(long, env = "MOQ_RTC_TLS_KEY", requires = "tls_cert")]
	tls_key: Option<PathBuf>,

	/// Public UDP socket address(es) to advertise as ICE host candidates.
	/// Repeat the flag for multi-homed deployments. Falls back to the
	/// kernel-picked address when empty (loopback testing only).
	#[arg(long = "ice-candidate", env = "MOQ_RTC_ICE_CANDIDATE", value_delimiter = ',')]
	ice_candidates: Vec<SocketAddr>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let cli = Cli::parse();
	cli.log.init();

	let client = cli.client.init().context("failed to init moq client")?;

	// Two origins so WHIP ingest and WHEP egress see independent views of
	// the upstream relay. The publisher feeds the relay; the subscriber
	// reads from it.
	let publisher = moq_net::Origin::random().produce();
	let subscriber = moq_net::Origin::random().produce();
	let subscriber_consumer = subscriber.consume();

	let reconnect = client
		.with_publish(publisher.consume())
		.with_consume(subscriber)
		.reconnect(cli.relay.clone());

	let config = GatewayConfig {
		ice_candidates: cli.ice_candidates,
	};
	let gateway = Gateway::new(config, publisher, subscriber_consumer);

	let app = Router::new()
		.nest("/whip", gateway.whip_router())
		.nest("/whep", gateway.whep_router())
		.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));

	let bind = cli.listen;
	tracing::info!(%bind, relay = %cli.relay, "starting moq-rtc");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	let serve = serve(app, bind, cli.tls_cert, cli.tls_key);

	tokio::select! {
		res = serve => res,
		res = reconnect.closed() => res,
		_ = tokio::signal::ctrl_c() => Ok(()),
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
		_ => {
			axum_server::bind(bind).serve(service).await?;
		}
	}
	Ok(())
}
