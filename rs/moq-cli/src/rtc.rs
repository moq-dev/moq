//! WebRTC (WHIP/WHEP) endpoints. Direction picks the HTTP role:
//! - import `--listen` = WHIP server (accept publishes); `--connect` = WHEP client (pull).
//! - export `--listen` = WHEP server (serve plays); `--connect` = WHIP client (push).

use std::net::SocketAddr;

use anyhow::Context;
use axum::http::Method;
use hang::moq_net;
use hang::moq_net::AsPath;
use url::Url;

use crate::moq::{ImportTarget, notify_ready};

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

/// HTTP and ICE settings shared by the WHIP and WHEP listeners.
pub struct Listen {
	/// HTTP address to bind.
	pub addr: SocketAddr,

	/// Shared UDP socket for ICE and media.
	pub udp_bind: SocketAddr,

	/// Public UDP addresses advertised as ICE host candidates.
	pub public_addr: Vec<SocketAddr>,

	/// Browser CORS policy for the HTTP listener.
	pub cors: crate::web::Cors,
}

/// WHIP server: accept incoming WebRTC publishes into the Origin as `target.name` (import).
pub async fn listen_import(target: ImportTarget, listen: Listen) -> anyhow::Result<()> {
	let publisher = scope_producer(&target.origin, &target.name)?;
	let mut config = server_config(&listen);
	config.max_age = target.max_age;
	let server = moq_rtc::Server::new(config, publisher, target.origin.consume());
	serve(server.publish_router(), "WHIP", listen).await
}

/// WHEP server: serve WebRTC plays of `name` from the Origin (export).
pub async fn listen_export(origin: moq_net::origin::Consumer, name: String, listen: Listen) -> anyhow::Result<()> {
	let subscriber = origin
		.scope(&[name.as_path()])
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))?;
	// A WHEP server only reads; it still needs a publisher handle for the shared
	// glue, so hand it an unused, empty Origin producer.
	let publisher = moq_tokio::origin::spawn(moq_net::Origin::random());
	let server = moq_rtc::Server::new(server_config(&listen), publisher, subscriber);
	serve(server.subscribe_router(), "WHEP", listen).await
}

/// Restrict a producer to the single broadcast `name` so a WHIP peer can only publish it.
fn scope_producer(origin: &moq_net::origin::Producer, name: &str) -> anyhow::Result<moq_net::origin::Producer> {
	origin
		.scope(&[name.as_path()])
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))
}

/// WHEP client: pull a remote broadcast into the Origin under `target.name` (import).
pub async fn connect_import(target: ImportTarget, url: Url) -> anyhow::Result<()> {
	let name = &target.name;
	let producer = target
		.origin
		.create_broadcast(name, moq_net::broadcast::Route::new().with_announce(true))
		.context("failed to create broadcast")?;

	tracing::info!(%url, %name, "WHEP client pulling");
	notify_ready();

	let mut config = moq_rtc::client::Config::default();
	config.max_age = target.max_age;
	let client = moq_rtc::Client::new(config);
	Ok(client.subscribe(url, producer).await?)
}

/// WHIP client: push a broadcast from the Origin to a remote (export).
pub async fn connect_export(origin: moq_net::origin::Consumer, url: Url, name: String) -> anyhow::Result<()> {
	// Confirm the broadcast is reachable (and wait for it to be announced) before dialing;
	// the egress re-resolves it (and any referenced sibling broadcast) through the origin.
	origin
		.announced_broadcast(&name)
		.await
		.with_context(|| format!("origin closed before broadcast `{name}` was announced"))?;

	tracing::info!(%url, %name, "WHIP client pushing");
	notify_ready();

	let client = moq_rtc::Client::new(moq_rtc::client::Config::default());
	Ok(client.publish(url, origin, &name).await?)
}

fn server_config(listen: &Listen) -> moq_rtc::server::Config {
	let mut config = moq_rtc::server::Config::default();
	config.udp_bind = listen.udp_bind;
	config.ice_candidates.clone_from(&listen.public_addr);
	config
}

async fn serve(router: axum::Router, role: &str, listen: Listen) -> anyhow::Result<()> {
	let cors = listen
		.cors
		.layer([Method::POST, Method::PATCH, Method::DELETE, Method::OPTIONS])?;
	let app = router.layer(cors);
	let listener = moq_tokio::bind::tcp(listen.addr)?;

	tracing::info!(listen = %listen.addr, role, "serving WebRTC");
	notify_ready();

	crate::web::serve(listener, app, None).await
}
