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

	config.client.quic.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);
	config.server.quic.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);

	let mtls_enabled = !config.server.tls.root.is_empty();

	#[allow(unused_mut)]
	let mut server = config.server.init()?;
	let client = config.client.clone().init()?;

	// `None` for a stream-only server (no QUIC); any other error is real.
	let addr = match server.local_addr() {
		Ok(addr) => Some(addr),
		Err(moq_native::Error::NoBackend(_)) => None,
		Err(err) => return Err(err).context("failed to resolve the QUIC bind address"),
	};

	#[cfg(feature = "iroh")]
	let (server, client) = match config.iroh.bind(&config.client.quic).await? {
		Some(iroh) => (server.with_iroh(iroh.clone()), client.with_iroh(iroh)),
		None => (server, client),
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
		// mTLS-only: no JWT/public source, but `--auth-mtls-tier` still applies.
		Auth::default().with_mtls_tier(config.auth.mtls_tier.clone())
	} else {
		config.auth.init(&config.client.tls).await?
	};

	let cache = config.cache.init()?;
	let cluster = Cluster::new(config.cluster)?
		.with_cache(cache)
		.with_client(client)
		.with_client_tls(config.client.tls.build()?);
	// Keep the producer alive for the whole run: its publish task stops when
	// the last clone drops. The cluster only needs the counter registry.
	let stats = config.stats.build(cluster.origin.clone());
	let cluster = cluster.with_stats(stats.registry().clone());

	// Internal (ops) listener (plain HTTP, opt-in via `--internal-listen`) for
	// /metrics + /health, separate from the customer-facing web server. No-op
	// when unconfigured.
	let internal = Internal::new(config.internal, cluster.stats.clone());

	// Graceful shutdown: the first signal drains every accepted session with a
	// GOAWAY; a second signal (or the drain window elapsing) exits.
	let drain_timeout = std::time::Duration::from_secs(config.drain_timeout.unwrap_or(DEFAULT_DRAIN_TIMEOUT_SECS));
	let (shutdown_trigger, shutdown) = Shutdown::new(drain_timeout);

	// Create a web server too. mTLS for HTTPS is opt-in via `--web-https-root`.
	let web =
		Web::new(auth.clone(), cluster.clone(), server.certificates(), config.web).with_shutdown(shutdown.clone());

	match addr {
		Some(addr) => tracing::info!(%addr, "listening"),
		None => tracing::info!("listening (stream transports only)"),
	}

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
		Err(err) = internal.run() => return Err(err).context("internal server failed"),
		Err(err) = serve(server, cluster, auth, shutdown) => return Err(err).context("server failed"),
		Err(err) = jemalloc => return Err(err).context("jemalloc profiler failed"),
		res = drain_on_signal(shutdown_trigger, drain_timeout) => return res,
		else => Ok(())
	}
}

/// Two-stage shutdown: the first signal fires the drain broadcast (every session
/// sends GOAWAY and waits for its peer to leave); the second signal, or the
/// drain window elapsing, exits the process.
async fn drain_on_signal(trigger: ShutdownTrigger, window: std::time::Duration) -> anyhow::Result<()> {
	shutdown_signal().await?;
	tracing::info!(
		?window,
		"shutdown signal received; draining sessions (signal again to exit immediately)"
	);
	trigger.start();

	// One extra second past the window so per-session force-closes fire first,
	// giving every peer a proper GoawayTimeout instead of a dropped transport.
	let grace = window + std::time::Duration::from_secs(1);
	tokio::select! {
		res = shutdown_signal() => {
			res?;
			tracing::warn!("second shutdown signal; exiting immediately");
		}
		_ = tokio::time::sleep(grace) => tracing::info!("drain window elapsed; exiting"),
	}
	Ok(())
}

/// Resolve on a shutdown request: SIGINT (ctrl-c) or, on unix, SIGTERM (what
/// systemd and most process supervisors send on stop).
async fn shutdown_signal() -> anyhow::Result<()> {
	#[cfg(unix)]
	{
		let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
			.context("failed to listen for SIGTERM")?;
		tokio::select! {
			res = tokio::signal::ctrl_c() => res.context("failed to listen for SIGINT")?,
			_ = term.recv() => {}
		}
		Ok(())
	}
	#[cfg(not(unix))]
	{
		tokio::signal::ctrl_c().await.context("failed to listen for shutdown")
	}
}

async fn serve(mut server: moq_native::Server, cluster: Cluster, auth: Auth, shutdown: Shutdown) -> anyhow::Result<()> {
	let mut conn_id = 0;

	while let Some(request) = server.accept().await {
		let conn = Connection {
			id: conn_id,
			request,
			cluster: cluster.clone(),
			auth: auth.clone(),
			shutdown: shutdown.clone(),
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
