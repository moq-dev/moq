//! HLS endpoints: pull a remote playlist into MoQ (import), or serve HLS and
//! DASH over HTTP from MoQ broadcasts (export), fetching media groups on demand.

use std::net::SocketAddr;

use anyhow::Context;
use axum::http::Method;
use hang::moq_net;
use hang::moq_net::AsPath;

use crate::moq::notify_ready;

/// HLS import (pull a remote playlist) args.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ImportArgs {
	/// Playlist URL (http/https) or local file path.
	pub playlist: String,
}

/// HLS export (serve over HTTP) args.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct ExportArgs {
	/// HTTP listener for the HLS endpoints.
	#[usage(long, default = "[::]:8089")]
	pub listen: SocketAddr,

	/// TLS certificates, keys, self-signed generation, and optional mTLS roots.
	#[usage(flatten)]
	pub tls: moq_tokio::tls::Listen,

	/// Minimum media listed in each rendition's playlist window. Keep it within the
	/// relay's group-cache retention, since segments are fetched from there on request.
	#[usage(long, default = "16s")]
	pub window: moq_tokio::Duration,

	/// Browser CORS policy for the HLS listener.
	#[usage(flatten)]
	pub cors: crate::web::Cors,
}

/// Pull a remote HLS/LL-HLS playlist (URL or file path) into the Origin under `name`.
pub async fn import(
	origin: &moq_net::origin::Producer,
	name: String,
	playlist: String,
	max_age: Option<std::time::Duration>,
) -> anyhow::Result<()> {
	let mut producer = origin
		.create_broadcast(&name, moq_net::broadcast::Route::new().with_announce(true))
		.context("failed to create broadcast")?;

	// Create catalog tracks before the broadcast becomes visible so a subscriber
	// can consume the catalog as soon as it observes the announcement.
	let config = moq_mux::catalog::Config::default().with_max_age(max_age);
	let catalog = moq_mux::catalog::Producer::with_config(&mut producer, config)?;

	let mut importer = moq_hls::import::Import::new(producer, catalog, moq_hls::import::Config::new(playlist))?;

	tracing::info!(%name, "importing HLS");

	importer.init().await?;
	notify_ready();
	Ok(importer.run().await?)
}

/// Serve HLS and DASH over HTTP for the single broadcast `name` (reached at
/// `/<name>/master.m3u8` and `/<name>/manifest.mpd`); other broadcasts in the
/// Origin are not served.
pub async fn export(origin: moq_net::origin::Consumer, args: ExportArgs, name: String) -> anyhow::Result<()> {
	let scoped = origin
		.scope(&[name.as_path()])
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))?;

	let mut config = moq_hls::export::Config::default();
	config.window = args.window.into_std();
	let server = moq_hls::Server::new(scoped, config);
	let app = server.router().layer(args.cors.layer([Method::GET])?);

	let tls = if args.tls.cert.is_empty() && args.tls.generate.is_empty() {
		None
	} else {
		let alpn = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
		Some(args.tls.server_config(alpn)?)
	};

	let listener = moq_tokio::bind::tcp(args.listen)?;

	tracing::info!(listen = %args.listen, "serving HLS");
	notify_ready();

	crate::web::serve(listener, app, tls).await
}
