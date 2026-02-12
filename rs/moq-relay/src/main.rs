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

pub use auth::*;
pub use cluster::*;
pub use config::*;
pub use connection::*;
pub use web::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let config = Config::load()?;

	let addr = config.server.bind.unwrap_or("[::]:443".parse().unwrap());

	#[allow(unused_mut)]
	let mut server = config.server.init()?;
	let client = config.client.init()?;

	#[cfg(feature = "iroh")]
	let (mut server, client) = {
		let iroh = config.iroh.bind().await?;
		(server.with_iroh(iroh.clone()), client.with_iroh(iroh))
	};

	let auth = config.auth.init().await?;

	let cluster = Cluster::new(config.cluster, client);
	let cloned = cluster.clone();
	tokio::spawn(async move { cloned.run().await.expect("cluster failed") });

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

	tokio::spawn(async move {
		web.run().await.expect("failed to run web server");
	});

	tracing::info!(%addr, "listening");

	#[cfg(unix)]
	// Notify systemd that we're ready after all initialization is complete
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

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
			let err = conn.run().await;
			if let Err(err) = err {
				tracing::warn!(%err, "connection closed");
			}
		});
	}

	Ok(())
}
