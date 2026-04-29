use moq_relay::*;

use anyhow::Context;

#[cfg(feature = "jemalloc")]
#[global_allocator]
static ALLOC: moq_native::jemalloc::tikv_jemallocator::Jemalloc = moq_native::jemalloc::tikv_jemallocator::Jemalloc;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
	// TODO: It would be nice to remove this and rely on feature flags only.
	// However, some dependency is pulling in `ring` and I don't know why, so meh for now.
	rustls::crypto::aws_lc_rs::default_provider()
		.install_default()
		.expect("failed to install default crypto provider");

	let mut config = Config::load()?;

	config.client.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);
	config.server.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);

	let mtls_enabled = !config.server.tls.root.is_empty();

	#[allow(unused_mut)]
	let mut server = config.server.init()?;
	let client = config.client.clone().init()?;

	let addr = server.local_addr()?;

	#[cfg(feature = "iroh")]
	let (server, client) = {
		let iroh = config.iroh.bind().await?;
		(server.with_iroh(iroh.clone()), client.with_iroh(iroh))
	};

	// Reject configs where neither JWT nor mTLS can authenticate anyone.
	if config.auth.is_empty() {
		anyhow::ensure!(
			mtls_enabled,
			"no auth-key, auth-key-dir, public path, or server tls.root configured; \
			 nobody can authenticate"
		);
		tracing::warn!("no JWT/public auth configured; only mTLS peers will be accepted");
	}

	let auth = if config.auth.is_empty() {
		Auth::default()
	} else {
		config.auth.init().await?
	};

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
	let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

	#[cfg(feature = "jemalloc")]
	let jemalloc = moq_native::jemalloc::run();
	#[cfg(not(feature = "jemalloc"))]
	let jemalloc = std::future::pending::<anyhow::Result<()>>();

	tokio::select! {
		Err(err) = cluster.clone().run() => return Err(err).context("cluster failed"),
		Err(err) = web.run() => return Err(err).context("web server failed"),
		Err(err) = serve(server, cluster, auth) => return Err(err).context("server failed"),
		Err(err) = jemalloc => return Err(err).context("jemalloc profiler failed"),
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
