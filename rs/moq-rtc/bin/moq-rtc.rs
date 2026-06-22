//! `moq-rtc` binary.
//!
//! Subcommand structure mirrors `moq-cli`: globals first
//! (`--relay`, `--broadcast`), then HTTP role (`client` / `server`), then
//! direction (`publish` / `subscribe`). The 2x2 is:
//!
//! - `server publish` -- WHIP server, ingest from a remote publisher into MoQ.
//! - `server subscribe` -- WHEP server, egress a MoQ broadcast to remote subscribers.
//! - `client subscribe` -- WHEP client, pull a remote WHEP feed into MoQ.
//! - `client publish` -- WHIP client, push a MoQ broadcast to a remote WHIP endpoint.

use std::net::SocketAddr;
use std::path::PathBuf;

use anyhow::Context;
use axum::Router;
use clap::{Parser, Subcommand};
use moq_rtc::{Client, Server};
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

	/// URL of the upstream MoQ relay where ingested broadcasts get
	/// published and from which egressed broadcasts are pulled.
	#[arg(long, env = "MOQ_RTC_RELAY")]
	relay: Url,

	/// Broadcast name on the MoQ relay this gateway binds to. For
	/// `server publish` this is the path that arrives at the WHIP
	/// endpoint; for the other three it's a single name pinned to the
	/// gateway.
	#[arg(long, alias = "name", env = "MOQ_RTC_BROADCAST")]
	broadcast: String,

	/// Public UDP socket address(es) to advertise as ICE host candidates.
	/// Repeat the flag (or comma-separate) to advertise both an IPv4 and
	/// an IPv6 candidate on a dual-stack deployment. When empty, the
	/// session relies on str0m discovering peer-reflexive candidates via
	/// STUN binding requests, which is enough for most NAT scenarios.
	#[arg(long, env = "MOQ_RTC_PUBLIC_ADDR", value_delimiter = ',')]
	public_addr: Vec<SocketAddr>,

	#[command(subcommand)]
	role: Role,
}

#[derive(Subcommand, Clone)]
enum Role {
	/// Dial out: act as an HTTP client and POST an SDP offer to a remote
	/// WHIP or WHEP URL.
	Client {
		/// Remote WHIP or WHEP resource URL.
		#[arg(long, env = "MOQ_RTC_URL")]
		url: Url,

		#[command(subcommand)]
		direction: Direction,
	},
	/// Listen: accept incoming WHIP or WHEP HTTP requests.
	Server {
		/// HTTP listener for the WHIP/WHEP endpoints.
		#[arg(long, env = "MOQ_RTC_LISTEN", default_value = "[::]:8088")]
		listen: SocketAddr,

		/// UDP socket the shared WebRTC media mux binds to. All WHIP/WHEP
		/// sessions share this one port (demuxed by ICE ufrag), so open just
		/// this in the firewall. `0.0.0.0:0` lets the OS pick (loopback/dev).
		#[arg(long, env = "MOQ_RTC_UDP_BIND", default_value = "0.0.0.0:0")]
		udp_bind: SocketAddr,

		/// Optional TLS cert (PEM). Requires `--tls-key`.
		#[arg(long, env = "MOQ_RTC_TLS_CERT", requires = "tls_key")]
		tls_cert: Option<PathBuf>,

		#[arg(long, env = "MOQ_RTC_TLS_KEY", requires = "tls_cert")]
		tls_key: Option<PathBuf>,

		#[command(subcommand)]
		direction: Direction,
	},
}

#[derive(Subcommand, Clone, Copy, Debug)]
enum Direction {
	/// WHIP (publish protocol): RTP flows toward the WHIP-server endpoint.
	Publish,
	/// WHEP (subscribe protocol): RTP flows away from the WHEP-server endpoint.
	Subscribe,
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
		broadcast,
		public_addr,
		role,
	} = Cli::parse();
	log.init()?;

	let moq_client = moq_client.init().context("failed to init moq client")?;

	// Two origins so ingest and egress see independent views of the
	// upstream relay. The publisher feeds the relay; the subscriber
	// reads from it.
	let publisher = moq_net::Origin::random().produce();
	let subscriber = moq_net::Origin::random().produce();
	let subscriber_consumer = subscriber.consume();

	let reconnect = moq_client
		.with_publisher(&publisher)
		.with_subscriber(subscriber)
		.reconnect(relay.clone());

	tracing::info!(%relay, %broadcast, "starting moq-rtc");

	#[cfg(unix)]
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	let driver = run_role(role, &broadcast, public_addr, publisher, subscriber_consumer);

	tokio::select! {
		res = driver => res,
		res = reconnect.closed() => res.map_err(Into::into),
		_ = tokio::signal::ctrl_c() => Ok(()),
	}
}

async fn run_role(
	role: Role,
	broadcast: &str,
	public_addr: Vec<SocketAddr>,
	publisher: moq_net::OriginProducer,
	subscriber: moq_net::OriginConsumer,
) -> anyhow::Result<()> {
	match role {
		Role::Server {
			listen,
			udp_bind,
			tls_cert,
			tls_key,
			direction,
		} => {
			run_server(
				public_addr,
				udp_bind,
				publisher,
				subscriber,
				listen,
				tls_cert,
				tls_key,
				direction,
			)
			.await
		}
		Role::Client { url, direction } => {
			run_client(broadcast, public_addr, publisher, subscriber, url, direction).await
		}
	}
}

#[allow(clippy::too_many_arguments)]
async fn run_server(
	public_addr: Vec<SocketAddr>,
	udp_bind: SocketAddr,
	publisher: moq_net::OriginProducer,
	subscriber: moq_net::OriginConsumer,
	listen: SocketAddr,
	tls_cert: Option<PathBuf>,
	tls_key: Option<PathBuf>,
	direction: Direction,
) -> anyhow::Result<()> {
	let config = moq_rtc::server::Config {
		ice_candidates: public_addr,
		udp_bind,
	};
	let server = Server::new(config, publisher, subscriber);

	let app = match direction {
		Direction::Publish => Router::new().merge(server.publish_router()),
		Direction::Subscribe => Router::new().merge(server.subscribe_router()),
	};
	let app = app.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));

	tracing::info!(%listen, ?direction, "moq-rtc server listening");
	serve(app, listen, tls_cert, tls_key).await
}

async fn run_client(
	broadcast_name: &str,
	public_addr: Vec<SocketAddr>,
	publisher: moq_net::OriginProducer,
	subscriber: moq_net::OriginConsumer,
	url: Url,
	direction: Direction,
) -> anyhow::Result<()> {
	let config = moq_rtc::client::Config {
		ice_candidates: public_addr,
	};
	let client = Client::new(config);

	match direction {
		Direction::Subscribe => {
			// WHEP client: receive remote RTP, publish it as `broadcast_name`.
			let broadcast = moq_net::BroadcastInfo::new().produce();
			let consumer = broadcast.consume();
			let _publish = publisher
				.publish_broadcast(broadcast_name, &consumer)
				.context("failed to publish broadcast")?;
			client.subscribe(url, broadcast).await?;
		}
		Direction::Publish => {
			// WHIP client: read the local broadcast, push as RTP to remote.
			// Once the per-codec re-packetizer lands, this should poll
			// `subscriber.announced()` to await the broadcast rather than
			// erroring on first-miss.
			let broadcast = async { subscriber.request_broadcast(broadcast_name)?.await }
				.await
				.map_err(|_| anyhow::anyhow!("broadcast {} not announced", broadcast_name))?;
			client.publish(url, broadcast).await?;
		}
	}

	// `client.subscribe` spawns the session in the background; block on
	// ctrl-c instead of returning so the binary stays up.
	tokio::signal::ctrl_c().await?;
	Ok(())
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
		// clap's `requires` already gates this at parse time; the explicit
		// arm is belt-and-suspenders in case someone strips the attribute.
		(Some(_), None) | (None, Some(_)) => anyhow::bail!("--tls-cert and --tls-key must be set together"),
	}
	Ok(())
}
