//! MoQ relay server connecting publishers to subscribers.
//!
//! Content-agnostic relay that works with any live data, not just media.
//!
//! Features:
//! - Clustering: connect multiple relays for global distribution
//! - Authentication: JWT-based access control via [`moq_token`]
//! - WebSocket fallback: for restrictive networks
//! - HTTP API: health checks and metrics via [`Web`]

mod auth;
mod cluster;
mod config;
mod connection;
mod web;
#[cfg(feature = "websocket")]
mod websocket;

/// The relay needs higher stream limits than the library default
/// to handle many concurrent subscriptions across connections.
const DEFAULT_MAX_STREAMS: u64 = 10_000;

pub use auth::*;
pub use cluster::*;
pub use config::*;
pub use connection::*;
pub use web::*;

use anyhow::Context;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let mut config = Config::load()?;

	let addr = config.server.bind.unwrap_or("[::]:443".parse().unwrap());

	config.client.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);
	config.server.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);

	#[allow(unused_mut)]
	let mut server = config.server.init()?;
	let client = config.client.init()?;

	#[cfg(feature = "iroh")]
	let (server, client) = {
		let iroh = config.iroh.bind().await?;
		(server.with_iroh(iroh.clone()), client.with_iroh(iroh))
	};

	let auth = config.auth.init().await?;

	let cluster = Cluster::new(config.cluster, client);

	// Create a web server too.
	let web = Web::new(
		WebState {
			auth: auth.clone(),
			cluster: cluster.clone(),
			tls_info: server.tls_info(),
			conn_id: Default::default(),
		},
		config.web,
	);

	tracing::info!(%addr, "listening");

	#[cfg(unix)]
	// Notify systemd that we're ready after all initialization is complete
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	tokio::select! {
		Err(err) = cluster.clone().run() => return Err(err).context("cluster failed"),
		Err(err) = web.run() => return Err(err).context("web server failed"),
		Err(err) = serve(server, cluster, auth) => return Err(err).context("server failed"),
		else => Ok(()),
	}
}

async fn serve(mut server: moq_native::Server, cluster: Cluster, auth: Auth) -> anyhow::Result<()> {
	let mut conn_id = 0;

	while let Some(request) = server.accept().await {
		let conn = Connection {
			id: conn_id,
			request,
			cluster: cluster.clone(),
			auth: auth.clone(),
		};

		conn_id += 1;
		tokio::spawn(async move {
			if let Err(err) = conn.run().await {
				tracing::warn!(%err, "connection closed");
			}
		});
	}

	anyhow::bail!("stopped accepting connections")
}
