//! WebRTC (WHIP/WHEP) endpoints. Direction picks the HTTP role:
//! - import `--listen` = WHIP server (accept publishes); `--connect` = WHEP client (pull).
//! - export `--listen` = WHEP server (serve plays); `--connect` = WHIP client (push).

use std::net::SocketAddr;

use anyhow::Context;
use axum::http::Method;
use hang::moq_net;
use hang::moq_net::AsPath;
use url::Url;

use crate::moq::notify_ready;

/// WebRTC endpoint args: exactly one of `--connect` (WHIP/WHEP client) /
/// `--listen` (WHIP/WHEP server). The parent direction picks WHIP vs WHEP.
#[derive(clap::Args, Clone)]
#[command(group = clap::ArgGroup::new("rtc-mode").required(true).multiple(false).args(["rtc-connect", "rtc-listen"]))]
pub struct Args {
	/// Dial a remote WHIP/WHEP endpoint URL.
	#[arg(id = "rtc-connect", long = "connect", value_name = "URL")]
	pub connect: Option<Url>,

	/// Bind an HTTP listener for WHIP/WHEP, scoped to the single `--broadcast`
	/// (peers reach it at `http://host:port/<broadcast>`).
	#[arg(id = "rtc-listen", long = "listen", value_name = "ADDR")]
	pub listen: Option<SocketAddr>,

	/// Shared UDP socket for ICE/media (one port for all sessions).
	#[arg(long, requires = "rtc-listen", default_value = "[::]:0")]
	pub udp_bind: SocketAddr,

	/// Public UDP address(es) advertised as ICE host candidates (repeatable).
	#[arg(long, requires = "rtc-listen")]
	pub public_addr: Vec<SocketAddr>,

	/// Browser CORS policy for the WHIP/WHEP listener.
	#[command(flatten)]
	pub cors: crate::web::Cors,
}

/// WHIP server: accept incoming WebRTC publishes into the Origin as `name` (import).
pub async fn listen_import(
	origin: moq_net::OriginProducer,
	listen: SocketAddr,
	udp_bind: SocketAddr,
	public_addr: Vec<SocketAddr>,
	cors: crate::web::Cors,
	name: String,
) -> anyhow::Result<()> {
	let publisher = scope_producer(&origin, &name)?;
	let server = server(publisher, origin.consume(), udp_bind, public_addr);
	serve(server.publish_router(), listen, "WHIP", cors).await
}

/// WHEP server: serve WebRTC plays of `name` from the Origin (export).
pub async fn listen_export(
	origin: moq_net::OriginConsumer,
	listen: SocketAddr,
	udp_bind: SocketAddr,
	public_addr: Vec<SocketAddr>,
	cors: crate::web::Cors,
	name: String,
) -> anyhow::Result<()> {
	let subscriber = origin
		.scope(&[name.as_path()])
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))?;
	// A WHEP server only reads; it still needs a publisher handle for the shared
	// glue, so hand it an unused, empty Origin producer.
	let publisher = moq_net::Origin::random().produce();
	let server = server(publisher, subscriber, udp_bind, public_addr);
	serve(server.subscribe_router(), listen, "WHEP", cors).await
}

/// Restrict a producer to the single broadcast `name` so a WHIP peer can only publish it.
fn scope_producer(origin: &moq_net::OriginProducer, name: &str) -> anyhow::Result<moq_net::OriginProducer> {
	origin
		.scope(&[name.as_path()])
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))
}

/// WHEP client: pull a remote broadcast into the Origin under `name` (import).
pub async fn connect_import(origin: moq_net::OriginProducer, url: Url, name: String) -> anyhow::Result<()> {
	let producer = moq_net::BroadcastInfo::new().produce();
	// Hold the RAII announcement for the lifetime of the pull.
	let _announce = origin
		.publish_broadcast(&name, producer.consume())
		.context("failed to publish broadcast")?;

	tracing::info!(%url, %name, "WHEP client pulling");
	notify_ready();

	let client = moq_rtc::Client::new(moq_rtc::client::Config::default());
	Ok(client.subscribe(url, producer).await?)
}

/// WHIP client: push a broadcast from the Origin to a remote (export).
pub async fn connect_export(origin: moq_net::OriginConsumer, url: Url, name: String) -> anyhow::Result<()> {
	let broadcast = origin
		.announced_broadcast(&name)
		.await
		.with_context(|| format!("origin closed before broadcast `{name}` was announced"))?;

	tracing::info!(%url, %name, "WHIP client pushing");
	notify_ready();

	let client = moq_rtc::Client::new(moq_rtc::client::Config::default());
	Ok(client.publish(url, broadcast).await?)
}

fn server(
	publisher: moq_net::OriginProducer,
	subscriber: moq_net::OriginConsumer,
	udp_bind: SocketAddr,
	public_addr: Vec<SocketAddr>,
) -> moq_rtc::Server {
	let mut config = moq_rtc::server::Config::default();
	config.udp_bind = udp_bind;
	config.ice_candidates = public_addr;
	moq_rtc::Server::new(config, publisher, subscriber)
}

async fn serve(router: axum::Router, listen: SocketAddr, role: &str, cors: crate::web::Cors) -> anyhow::Result<()> {
	let cors = cors.layer([Method::POST, Method::PATCH, Method::DELETE, Method::OPTIONS])?;
	let app = router.layer(cors);
	let listener = moq_native::bind::tcp(listen)?;

	tracing::info!(%listen, role, "serving WebRTC");
	notify_ready();

	crate::web::serve(listener, app, None).await
}
