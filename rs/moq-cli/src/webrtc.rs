//! WebRTC (WHIP/WHEP) endpoints. Direction picks the HTTP role:
//! - import `--listen` = WHIP server (accept publishes); `--connect` = WHEP client (pull).
//! - export `--listen` = WHEP server (serve plays); `--connect` = WHIP client (push).

use std::net::SocketAddr;

use anyhow::Context;
use axum::http::Method;
use moq_tokio::RedactedUrl;
use url::Url;

use crate::moq::{ImportTarget, notify_ready};

/// WebRTC endpoint args: exactly one of `--connect` (WHIP/WHEP client) /
/// `--listen` (WHIP/WHEP server). The parent direction picks WHIP vs WHEP.
#[derive(usage_rs::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[usage(group("webrtc-mode", required))]
pub struct Args {
	/// Dial a remote WHIP/WHEP endpoint URL.
	#[usage(name = "webrtc-connect", long = "connect", value_name = "URL", group = "webrtc-mode")]
	pub connect: Option<Url>,

	/// Bind an HTTP listener for WHIP/WHEP, scoped to the single `--broadcast`
	/// (peers reach it at `http://host:port/<broadcast>`).
	#[usage(name = "webrtc-listen", long = "listen", value_name = "ADDR", group = "webrtc-mode")]
	pub listen: Option<SocketAddr>,

	/// Shared UDP socket for ICE/media (one port for all sessions), bound at startup.
	#[usage(long, requires = "--listen", default = "[::]:0")]
	pub udp_bind: SocketAddr,

	/// Public UDP address(es) advertised as ICE host candidates, as given (repeatable).
	/// Port 0 advertises the `--udp-bind` port.
	#[usage(long, requires = "--listen")]
	pub public_addr: Vec<SocketAddr>,

	/// Browser CORS policy for the WHIP/WHEP listener.
	#[usage(flatten)]
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
	let server = bind(&listen).await?;
	serve(server.publish_router(publisher, target.ingest), "WHIP", listen).await
}

/// WHEP server: serve WebRTC plays of `name` from the Origin (export).
pub async fn listen_export(origin: moq_net::origin::Consumer, name: String, listen: Listen) -> anyhow::Result<()> {
	let scope = moq_net::Patterns::from(
		moq_net::Pattern::subtree(&name).with_context(|| format!("invalid broadcast name `{name}`"))?,
	);
	let subscriber = origin
		.scope("", &scope)
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))?;
	let server = bind(&listen).await?;
	serve(server.subscribe_router(subscriber), "WHEP", listen).await
}

/// Restrict a producer to the single broadcast `name` so a WHIP peer can only publish it.
fn scope_producer(origin: &moq_net::origin::Producer, name: &str) -> anyhow::Result<moq_net::origin::Producer> {
	let scope = moq_net::Patterns::from(
		moq_net::Pattern::subtree(name).with_context(|| format!("invalid broadcast name `{name}`"))?,
	);
	origin
		.scope("", &scope)
		.with_context(|| format!("failed to scope origin to broadcast `{name}`"))
}

/// WHEP client: pull a remote broadcast into the Origin under `target.name` (import),
/// announced under `instance` once its catalog names a track.
pub async fn connect_import(target: ImportTarget, url: Url, instance: moq_net::Instance) -> anyhow::Result<()> {
	let name = &target.name;
	// Hidden until the catalog exists: a viewer or export that resolved the broadcast earlier
	// would find no catalog track. Held until the pull ends.
	let route = target
		.origin
		.route(name, moq_net::origin::RouteOptions::hidden().with_instance(instance, 0))
		.context("failed to create broadcast")?;
	let catalog = moq_mux::catalog::Producer::new(&mut route.broadcast().clone(), target.ingest.catalog())?;
	let client = moq_webrtc::Client::new(moq_webrtc::client::Config::default())?;

	tracing::info!(url = %RedactedUrl::new(&url), %name, "WHEP client pulling");
	notify_ready();

	let announce = async {
		let mut updates = catalog.consume()?;
		// The pull ends the catalog when its session ends, and reports why itself.
		if let Ok(Some(_)) = updates.next().await {
			route.announce().context("failed to announce broadcast")?;
			tracing::info!(%name, "publishing WHEP broadcast");
		}
		std::future::pending::<anyhow::Result<()>>().await
	};

	tokio::select! {
		end = client.pull(url, route.broadcast(), &catalog) => Ok(end?),
		err = announce => err,
	}
}

/// WHIP client: push a broadcast from the Origin to a remote (export).
pub async fn connect_export(origin: moq_net::origin::Consumer, url: Url, name: String) -> anyhow::Result<()> {
	// Wait for the broadcast to be announced before dialing; the egress resolves any
	// referenced sibling broadcast through the same origin.
	let broadcast = origin
		.follow(name.as_str(), None)?
		.available()
		.await
		.with_context(|| format!("failed to resolve broadcast `{name}`"))?
		.context("went offline before serving")?;
	let source = moq_mux::Source::new(origin, broadcast);

	tracing::info!(url = %RedactedUrl::new(&url), %name, "WHIP client pushing");
	notify_ready();

	let client = moq_webrtc::Client::new(moq_webrtc::client::Config::default())?;
	client.publish(url, source).await?;
	Ok(())
}

/// Bind the shared media socket, so a bad `--udp-bind` fails startup.
async fn bind(listen: &Listen) -> anyhow::Result<moq_webrtc::Server> {
	let mut config = moq_webrtc::server::Config::default();
	config.ice_candidates.clone_from(&listen.public_addr);
	moq_webrtc::Server::bind(listen.udp_bind, config)
		.await
		.with_context(|| format!("failed to bind --udp-bind {}", listen.udp_bind))
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

#[cfg(test)]
mod tests {
	use futures::FutureExt;

	use super::*;

	/// A WHEP pull keeps its broadcast hidden while the session negotiates, since nothing has
	/// published its catalog yet, and never announces a pull that fails before then.
	#[tokio::test]
	async fn a_whep_pull_hides_its_broadcast_until_the_catalog_exists() {
		let origin = moq_tokio::origin::spawn();
		let consumer = origin.consume();
		let mut announced = consumer.announced();
		// A WHEP endpoint that takes the offer and never answers it.
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let url = format!("http://{}/whep", listener.local_addr().unwrap())
			.parse()
			.unwrap();
		let target = ImportTarget {
			origin,
			name: "pulled".to_string(),
			ingest: moq_mux::Ingest::default(),
		};
		let pull = tokio::spawn(connect_import(target, url, moq_net::Instance::random()));

		let (offer, _) = listener.accept().await.unwrap();
		assert!(
			announced.next().now_or_never().is_none(),
			"announced before the catalog exists"
		);

		drop(offer);
		pull.await.unwrap().expect_err("the endpoint hung up without an answer");
		// The origin ends with the pull, so the only announcement left would be the pull's.
		assert!(
			announced.next().now_or_never().flatten().is_none(),
			"announced a failed pull"
		);
	}
}
