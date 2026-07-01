//! HLS / LL-HLS endpoints: pull a remote playlist into MoQ (import), or serve
//! HLS over HTTP from MoQ broadcasts (export).

use std::net::SocketAddr;
use std::time::Duration;

use hang::moq_net;
use tower_http::cors::{Any, CorsLayer};

use crate::moq::notify_ready;

/// Pull a remote HLS/LL-HLS playlist (URL or file path) into the Origin under `name`.
pub async fn import(origin: &moq_net::OriginProducer, name: String, playlist: String) -> anyhow::Result<()> {
	let mut producer = moq_net::Broadcast::new().produce();
	anyhow::ensure!(
		origin.publish_broadcast(&name, producer.consume()),
		"failed to publish broadcast"
	);

	let catalog = moq_mux::catalog::Producer::new(&mut producer)?;
	let mut importer = moq_hls::import::Import::new(producer, catalog, moq_hls::import::Config::new(playlist))?;

	tracing::info!(%name, "importing HLS");

	importer.init().await?;
	notify_ready();
	Ok(importer.run().await?)
}

/// Serve HLS/LL-HLS over HTTP from the Origin's broadcasts.
pub async fn export(
	origin: moq_net::OriginConsumer,
	listen: SocketAddr,
	tls: moq_native::tls::Server,
	part_target: Duration,
	window: Duration,
) -> anyhow::Result<()> {
	let config = moq_hls::export::Config {
		part_target,
		window,
		..Default::default()
	};
	let server = moq_hls::Server::new(origin, config);
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

	tracing::info!(%listen, "serving HLS");
	notify_ready();

	crate::web::serve(listener, app, tls).await
}
