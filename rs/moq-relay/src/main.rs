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

	// We drive shutdown ourselves (GOAWAY drain on the first Ctrl+C, force on the
	// second), so opt out of moq-native's built-in Ctrl+C-closes-everything handler.
	#[allow(unused_mut)]
	let mut server = config.server.init()?.with_ctrl_c_handler(false);
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

	let cluster = Cluster::new(config.cluster)
		.with_client(client)
		.with_client_tls(config.client.tls.build()?);
	let stats = config.stats.build(cluster.origin.clone());
	let cluster = cluster.with_stats(stats);

	// Create a web server too. mTLS for HTTPS is opt-in via `--web-https-root`.
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

	// Graceful-then-forceful shutdown. The first stop signal (SIGINT/Ctrl+C, or SIGTERM
	// from `systemctl stop`) drains; a second one forces. There's no handler for a hard
	// kill: SIGKILL is uncatchable, and that's the kernel-level backstop (systemd sends
	// it after TimeoutStopSec).
	let (drain_tx, drain_rx) = tokio::sync::watch::channel(false);
	let shutdown = async move {
		// Open the streams once so the second signal can't slip through a re-registration gap.
		let mut signals = ShutdownSignals::listen();

		signals.recv().await;
		tracing::info!("shutting down: sending GOAWAY and draining connections (signal again to force)");
		#[cfg(unix)]
		let _ = sd_notify::notify(&[sd_notify::NotifyState::Stopping]);
		let _ = drain_tx.send(true);

		signals.recv().await;
		tracing::warn!("forcing shutdown");
	};

	tokio::select! {
		Err(err) = cluster.clone().run() => Err(err).context("cluster failed"),
		Err(err) = web.run() => Err(err).context("web server failed"),
		res = serve(server, cluster, auth, drain_rx) => res.context("server failed"),
		Err(err) = jemalloc => Err(err).context("jemalloc profiler failed"),
		// Forced shutdown: dropping `server` (and the connection tasks) closes everything.
		_ = shutdown => Ok(()),
	}
}

/// Listens for OS stop signals. On Unix that's SIGINT (Ctrl+C) and SIGTERM (what
/// `systemctl stop` sends); elsewhere just Ctrl+C. Both streams are registered up
/// front so repeated signals are delivered reliably.
struct ShutdownSignals {
	#[cfg(unix)]
	sigint: tokio::signal::unix::Signal,
	#[cfg(unix)]
	sigterm: tokio::signal::unix::Signal,
}

impl ShutdownSignals {
	fn listen() -> Self {
		#[cfg(unix)]
		{
			use tokio::signal::unix::{SignalKind, signal};
			Self {
				sigint: signal(SignalKind::interrupt()).expect("failed to listen for SIGINT"),
				sigterm: signal(SignalKind::terminate()).expect("failed to listen for SIGTERM"),
			}
		}
		#[cfg(not(unix))]
		Self {}
	}

	/// Resolve on the next stop signal.
	async fn recv(&mut self) {
		#[cfg(unix)]
		tokio::select! {
			_ = self.sigint.recv() => {}
			_ = self.sigterm.recv() => {}
		}
		#[cfg(not(unix))]
		{
			// Windows: Ctrl+C only. A failed registration must not look like a signal.
			if tokio::signal::ctrl_c().await.is_err() {
				std::future::pending::<()>().await;
			}
		}
	}
}

async fn serve(
	mut server: moq_native::Server,
	cluster: Cluster,
	auth: Auth,
	mut drain: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
	let mut conn_id = 0;

	// Tracks in-flight connections: each task holds a clone of `active`, so
	// `active_rx.recv()` resolves to `None` only once every task has finished.
	let (active, mut active_rx) = tokio::sync::mpsc::channel::<()>(1);

	loop {
		let request = tokio::select! {
			request = server.accept() => request,
			// Stop accepting once draining begins; existing connections keep running.
			_ = drain.wait_for(|d| *d) => break,
		};

		let Some(request) = request else { break };

		let conn = Connection {
			id: conn_id,
			request,
			cluster: cluster.clone(),
			auth: auth.clone(),
		};

		conn_id += 1;
		let drain = drain.clone();
		let active = active.clone();
		tokio::spawn(async move {
			let _active = active;
			if let Err(err) = conn.run(drain).await {
				tracing::warn!(%err, "connection closed");
			}
		});
	}

	// No longer accepting. Wait for every in-flight connection to drain.
	drop(active);
	tracing::info!("waiting for connections to drain");
	let _ = active_rx.recv().await;
	tracing::info!("all connections drained");
	Ok(())
}
