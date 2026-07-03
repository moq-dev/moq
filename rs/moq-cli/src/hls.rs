//! HLS / LL-HLS endpoints: pull a remote playlist into MoQ (import), or serve
//! HLS over HTTP from MoQ broadcasts (export).

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use axum::http::Method;
use hang::moq_net;
use hang::moq_net::AsPath;

use crate::moq::notify_ready;

/// HLS import (pull a remote playlist) args.
#[derive(clap::Args, Clone)]
pub struct ImportArgs {
	/// Playlist URL (http/https) or local file path.
	pub playlist: String,
}

/// HLS export (serve over HTTP) args.
#[derive(clap::Args, Clone)]
pub struct ExportArgs {
	/// HTTP listener for the HLS endpoints.
	#[arg(long, default_value = "[::]:8089")]
	pub listen: SocketAddr,

	/// TLS certificates, keys, self-signed generation, and optional mTLS roots.
	#[command(flatten)]
	pub tls: moq_native::tls::Server,

	/// LL-HLS part target duration.
	#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
	pub part_target: Duration,

	/// Minimum media kept in each rendition's sliding window.
	#[arg(long, default_value = "16s", value_parser = humantime::parse_duration)]
	pub window: Duration,

	/// Maximum latency before skipping a stalled group. Generous by default so
	/// live GOPs aren't skipped while segments build.
	#[arg(long = "latency-max", default_value = "10s", value_parser = humantime::parse_duration)]
	pub latency_max: Duration,

	/// Browser CORS policy for the HLS listener.
	#[command(flatten)]
	pub cors: crate::web::Cors,
}

/// Pull a remote HLS/LL-HLS playlist (URL or file path) into the Origin under `name`.
pub async fn import(origin: &moq_net::OriginProducer, name: String, playlist: String) -> anyhow::Result<()> {
	let mut producer = moq_net::BroadcastInfo::new().produce();
	// Hold the RAII announcement for the lifetime of the import.
	let _announce = origin
		.publish_broadcast(&name, producer.consume())
		.context("failed to publish broadcast")?;

	let catalog = moq_mux::catalog::Producer::new(&mut producer)?;
	let mut importer = moq_hls::import::Import::new(producer, catalog, moq_hls::import::Config::new(playlist))?;

	tracing::info!(%name, "importing HLS");

	importer.init().await?;
	notify_ready();
	Ok(importer.run().await?)
}

/// Serve HLS/LL-HLS over HTTP for the single broadcast `name` (reached at
/// `/<name>/master.m3u8`); other broadcasts in the Origin are not served.
pub async fn export(origin: moq_net::OriginConsumer, args: ExportArgs, name: String) -> anyhow::Result<()> {
	let scoped = origin
		.scope(&[name.as_path()])
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))?;

	let config = moq_hls::export::Config {
		part_target: args.part_target,
		window: args.window,
		latency: args.latency_max,
		..Default::default()
	};
	let server = moq_hls::Server::new(scoped, config);
	let app = server.router().layer(args.cors.layer([Method::GET])?);

	let tls = if args.tls.cert.is_empty() && args.tls.generate.is_empty() {
		None
	} else {
		let alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
		Some(args.tls.server_config(alpn)?)
	};

	let listener = moq_native::bind::tcp(args.listen)?;

	tracing::info!(listen = %args.listen, "serving HLS");
	notify_ready();

	crate::web::serve(listener, app, tls).await
}
