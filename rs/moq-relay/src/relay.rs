//! High-level [`Relay`]: wires the building blocks ([`Cluster`], [`Web`],
//! [`Auth`], the QUIC server) into a runnable server.
//!
//! This is the recommended entry point both for the standalone binary (see
//! `main.rs`) and for embedding the relay in a larger process. Embedders build a
//! [`Relay`], spawn their own tasks against [`Relay::origin`] (the one
//! [`OriginProducer`] every session and cluster peer publishes into), and then
//! call [`Relay::run`]:
//!
//! ```no_run
//! # async fn example(config: moq_relay::Config) -> anyhow::Result<()> {
//! let relay = moq_relay::Relay::new(config).await?;
//! let origin = relay.origin().clone();
//! tokio::spawn(async move {
//!     // a worker consuming the shared origin in-process
//!     let _ = origin;
//! });
//! relay.run().await
//! # }
//! ```

use anyhow::Context;
use moq_net::OriginProducer;

use crate::{Auth, Cluster, Config, Connection, DEFAULT_MAX_STREAMS, Web, WebState};

/// A fully wired relay, ready to [`run`](Self::run).
///
/// Construct with [`Relay::new`]. Between construction and [`run`](Self::run),
/// [`origin`](Self::origin) exposes the shared [`OriginProducer`] so embedders
/// can attach in-process producers/consumers (workers) to the same broadcast
/// set the relay serves.
pub struct Relay {
	server: moq_native::Server,
	cluster: Cluster,
	auth: Auth,
	web: Web,
}

impl Relay {
	/// Build the relay stack from [`Config`]: the QUIC server + cluster client,
	/// authentication, the [`Cluster`] (and its origin + stats), and the web
	/// server. This is the wiring the binary previously inlined in `main`.
	///
	/// Async because binding the optional iroh endpoint is async.
	///
	/// Errors if no authentication is configured at all (neither JWT/public nor
	/// mTLS could authenticate anyone), or if any sub-config fails to initialize.
	pub async fn new(mut config: Config) -> anyhow::Result<Self> {
		config.client.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);
		config.server.max_streams.get_or_insert(DEFAULT_MAX_STREAMS);

		let mtls_enabled = !config.server.tls.root.is_empty();

		#[allow(unused_mut)]
		let mut server = config.server.init()?;
		let client = config.client.clone().init()?;

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

		let cluster = Cluster::new(config.cluster)?
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

		Ok(Self {
			server,
			cluster,
			auth,
			web,
		})
	}

	/// The shared [`OriginProducer`] every local session and cluster peer
	/// publishes into. Embedders clone this to attach in-process workers, scoped
	/// via [`OriginProducer::scope`] / [`OriginProducer::consume`].
	pub fn origin(&self) -> &OriginProducer {
		&self.cluster.origin
	}

	/// The relay's [`Cluster`], for embedders that need the full handle (e.g.
	/// to derive auth-scoped publishers/subscribers).
	pub fn cluster(&self) -> &Cluster {
		&self.cluster
	}

	/// Run the relay until a fatal error. Drives the cluster mesh, the web
	/// server, the QUIC accept loop, and (when the `jemalloc` feature is on) the
	/// heap profiler, returning the first error any of them produces.
	///
	/// Notifies systemd readiness (a no-op when not run under systemd) once the
	/// server is listening, so embedders inherit the `Type=notify` contract for
	/// free.
	pub async fn run(self) -> anyhow::Result<()> {
		let Relay {
			server,
			cluster,
			auth,
			web,
		} = self;

		let addr = server.local_addr()?;
		tracing::info!(%addr, "listening");

		#[cfg(unix)]
		// Notify systemd that we're ready after all initialization is complete.
		let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

		#[cfg(feature = "jemalloc")]
		let jemalloc = moq_native::jemalloc::run();
		#[cfg(not(feature = "jemalloc"))]
		let jemalloc = std::future::pending::<anyhow::Result<()>>();

		tokio::select! {
			Err(err) = cluster.clone().run() => Err(err).context("cluster failed"),
			Err(err) = web.run() => Err(err).context("web server failed"),
			Err(err) = serve(server, cluster, auth) => Err(err).context("server failed"),
			Err(err) = jemalloc => Err(err).context("jemalloc profiler failed"),
			else => Ok(()),
		}
	}
}

/// Accept incoming sessions and spawn a [`Connection`] task for each. Every
/// connection shares the one `cluster` (and thus its origin), so a publisher on
/// one session is immediately visible to subscribers (and embedder workers).
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
