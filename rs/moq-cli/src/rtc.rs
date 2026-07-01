//! WebRTC (WHIP/WHEP) endpoints. Direction picks the HTTP role:
//! - import `--listen` = WHIP server (accept publishes); `--connect` = WHEP client (pull).
//! - export `--listen` = WHEP server (serve plays); `--connect` = WHIP client (push).

use std::net::SocketAddr;

use hang::moq_net;
use tower_http::cors::{Any, CorsLayer};
use url::Url;

use crate::moq::notify_ready;

/// WHIP server: accept incoming WebRTC publishes into the Origin (import).
pub async fn listen_import(
	origin: moq_net::OriginProducer,
	listen: SocketAddr,
	udp_bind: SocketAddr,
	public_addr: Vec<SocketAddr>,
) -> anyhow::Result<()> {
	let server = server(origin.clone(), origin.consume(), udp_bind, public_addr);
	serve(server.publish_router(), listen, "WHIP").await
}

/// WHEP server: serve WebRTC plays from the Origin (export).
pub async fn listen_export(
	origin: moq_net::OriginConsumer,
	listen: SocketAddr,
	udp_bind: SocketAddr,
	public_addr: Vec<SocketAddr>,
) -> anyhow::Result<()> {
	// A WHEP server only reads; it still needs a publisher handle for the shared
	// glue, so hand it an unused, empty Origin producer.
	let publisher = moq_net::Origin::random().produce();
	let server = server(publisher, origin, udp_bind, public_addr);
	serve(server.subscribe_router(), listen, "WHEP").await
}

/// WHEP client: pull a remote broadcast into the Origin under `name` (import).
pub async fn connect_import(origin: moq_net::OriginProducer, url: Url, name: String) -> anyhow::Result<()> {
	let producer = moq_net::Broadcast::new().produce();
	anyhow::ensure!(
		origin.publish_broadcast(&name, producer.consume()),
		"failed to publish broadcast"
	);

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
		.ok_or_else(|| anyhow::anyhow!("origin closed before broadcast `{name}` was announced"))?;

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

async fn serve(router: axum::Router, listen: SocketAddr, role: &str) -> anyhow::Result<()> {
	let app = router.layer(CorsLayer::new().allow_origin(Any).allow_methods(Any).allow_headers(Any));
	let listener = moq_native::bind::tcp(listen)?;

	tracing::info!(%listen, role, "serving WebRTC");
	notify_ready();

	crate::web::serve(listener, app, None).await
}
