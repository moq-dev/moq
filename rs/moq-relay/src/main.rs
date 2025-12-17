mod auth;
mod cluster;
mod config;
mod connection;
mod web;

pub use auth::*;
pub use cluster::*;
pub use config::*;
pub use connection::*;
pub use web::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	let config = Config::load()?;

	let addr = config.server.bind.unwrap_or("[::]:443".parse().unwrap());
	let mut server = config.server.init()?;
	let client = config.client.init()?;
	let auth = config.auth.init()?;
	let fingerprints = server.fingerprints().to_vec();

	#[cfg(feature = "iroh")]
	let mut iroh = config.iroh.init_server().await?;

	let cluster = Cluster::new(config.cluster, client);
	let cloned = cluster.clone();
	tokio::spawn(async move { cloned.run().await.expect("cluster failed") });

	// Create a web server too.
	let web = Web::new(
		WebState {
			auth: auth.clone(),
			cluster: cluster.clone(),
			fingerprints,
			conn_id: Default::default(),
		},
		config.web,
	);

	tokio::spawn(async move {
		web.run().await.expect("failed to run web server");
	});

	tracing::info!(%addr, "listening");

	// Notify systemd that we're ready after all initialization is complete
	let _ = sd_notify::notify(true, &[sd_notify::NotifyState::Ready]);

	let accept_quic = {
		let auth = auth.clone();
		let cluster = cluster.clone();
		tokio::spawn(async move {
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
		})
	};

	#[cfg(feature = "iroh")]
	let accept_iroh = {
		let auth = auth.clone();
		let cluster = cluster.clone();
		tokio::spawn(async move {
			let mut conn_id = 0;
			while let Some(request) = iroh.accept().await {
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
		})
	};

	futures::future::join_all([
		accept_quic,
		#[cfg(feature = "iroh")]
		accept_iroh,
	])
	.await;

	Ok(())
}
