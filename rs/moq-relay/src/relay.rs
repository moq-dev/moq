//! The assembled relay: every startup step, in one place, in the right order.
//!
//! [`Relay::load`] turns a [`Config`] into the running pieces (server, client,
//! auth, cluster, stats, internal + web listeners), and [`Relay::run`] drives
//! them. `main.rs` is a thin wrapper over the two.
//!
//! # Why this is a type and not just `main`
//!
//! An embedder that wants the relay PLUS its own workers - extra routes on the
//! web server, an in-process recorder against [`Cluster::origin`], another
//! listener in its own `select!` - used to have no choice but to copy `main`'s
//! body, because the assembly lived there. That copy then rots: this crate is
//! consumed as a git dependency, so a step added here arrives as a library
//! update with no call site on the other end, and the embedder silently keeps
//! the old behavior. Two such omissions ran on a production fleet for days
//! (an unbounded group cache whose `--cache-capacity` parsed and was dropped,
//! and a `/nodes` endpoint that answered `200` with an empty list), because
//! the config still parses either way and nothing errors.
//!
//! So the assembly is a public type: embedders call [`Relay::load`] and
//! destructure it, rather than reproducing the sequence. A step added to
//! `load` reaches every consumer on their next update, which is the whole
//! point.
//!
//! Ordering constraints live here too, where they can't be dropped in a copy.
//! [`Cluster::with_cache`] rebuilds the origin, so it runs before anything
//! derives a handle from it (the stats producer, and every session after).

use anyhow::Context;

use crate::{Auth, Cluster, Config, Connection, DEFAULT_MAX_STREAMS, Internal, Web};

/// A fully assembled relay: the listeners and the shared cluster behind them.
///
/// Fields are public so an embedder can take the pieces it needs and drive its
/// own event loop (mount extra routes on [`Self::web`], spawn workers against
/// [`Cluster::origin`], add listeners to its own `select!`). Use [`Self::run`]
/// when the stock behavior is what you want.
#[non_exhaustive]
pub struct Relay {
	/// The QUIC/WebTransport server, already bound. Feed it to [`serve`].
	pub server: moq_native::Server,

	/// The client used to dial cluster peers. Already handed to [`Self::cluster`];
	/// clone it for your own outbound dials so they share the connection config.
	pub client: moq_native::Client,

	/// The resolved auth policy (JWT/public sources, or mTLS-only).
	pub auth: Auth,

	/// The shared cluster: the origin every session and peer publishes into.
	pub cluster: Cluster,

	/// The stats producer. Keep it alive for as long as the relay runs: its
	/// publish task stops when the last clone drops.
	pub stats: moq_stats::Producer,

	/// The internal (ops) listener: `/metrics`, `/health`, `/nodes`. Inert
	/// unless `internal.listen` is configured.
	pub internal: Internal,

	/// The customer-facing web server. [`Web::routes`] + [`Web::serve`] to mount
	/// your own routes onto it; [`Web::run`] serves just the relay's own.
	pub web: Web,

	/// The QUIC bind address, or `None` for a stream-only server (no QUIC).
	pub addr: Option<std::net::SocketAddr>,
}

impl Relay {
	/// Assemble a relay from its configuration: bind the listeners, resolve
	/// auth, and build the cluster with its cache and stats attached.
	///
	/// This performs the side effects of starting up (binding sockets, reading
	/// key material, spawning the cache governor), so a returned `Relay` is
	/// ready to serve; nothing accepts a connection until [`Self::run`] (or
	/// [`serve`]) drives it.
	pub async fn load(mut config: Config) -> anyhow::Result<Self> {
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

		// Before any origin handle is derived: `with_cache` rebuilds the origin, so a
		// handle taken earlier would keep charging groups into the unbounded pool.
		let cache = config.cache.init()?;
		let cluster = Cluster::new(config.cluster)?
			.with_cache(cache)
			.with_client(client.clone())
			.with_client_tls(config.client.tls.build()?);
		// Keep the producer alive for the whole run: its publish task stops when
		// the last clone drops. The cluster only needs the counter registry.
		let stats = config.stats.build(cluster.origin.clone());
		let cluster = cluster.with_stats(stats.registry().clone());

		// Internal (ops) listener (plain HTTP, opt-in via `--internal-listen`) for
		// /metrics + /health + /nodes, separate from the customer-facing web server. No-op
		// when unconfigured.
		let internal = Internal::new(config.internal, cluster.stats.clone()).with_cluster(&cluster);

		// Create a web server too. mTLS for HTTPS is opt-in via `--web-https-root`.
		let web = Web::new(auth.clone(), cluster.clone(), server.certificates(), config.web);

		match addr {
			Some(addr) => tracing::info!(%addr, "listening"),
			None => tracing::info!("listening (stream transports only)"),
		}

		Ok(Relay {
			server,
			client,
			auth,
			cluster,
			stats,
			internal,
			web,
			addr,
		})
	}

	/// Serve until something fails: accept sessions, run the cluster, and serve
	/// both HTTP surfaces. Notifies systemd once everything is up.
	///
	/// Drive the pieces yourself instead when you have your own workers or
	/// routes to add; this is the stock loop.
	pub async fn run(self) -> anyhow::Result<()> {
		let Relay {
			server,
			auth,
			cluster,
			stats,
			internal,
			web,
			..
		} = self;

		// Held so the stats publish task outlives the setup that built it.
		let _stats = stats;

		#[cfg(unix)]
		// Notify systemd that we're ready after all initialization is complete
		let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

		#[cfg(feature = "jemalloc")]
		let jemalloc = moq_native::jemalloc::run();
		#[cfg(not(feature = "jemalloc"))]
		let jemalloc = std::future::pending::<anyhow::Result<()>>();

		tokio::select! {
			Err(err) = cluster.clone().run() => Err(err).context("cluster failed"),
			Err(err) = web.run() => Err(err).context("web server failed"),
			Err(err) = internal.run() => Err(err).context("internal server failed"),
			Err(err) = serve(server, cluster, auth) => Err(err).context("server failed"),
			Err(err) = jemalloc => Err(err).context("jemalloc profiler failed"),
			else => Ok(()),
		}
	}
}

/// Accept sessions off `server` until it stops, spawning a [`Connection`] task
/// for each.
///
/// Public because an embedder running its own `select!` still needs the accept
/// loop, and reimplementing it means re-deriving details like where a `conn` id
/// comes from.
pub async fn serve(mut server: moq_native::Server, cluster: Cluster, auth: Auth) -> anyhow::Result<()> {
	while let Some(request) = server.accept().await {
		let conn = Connection {
			// Shared with outbound cluster dials so one id space covers every session.
			id: cluster.next_connection_id(),
			request,
			cluster: cluster.clone(),
			auth: auth.clone(),
		};

		tokio::spawn(async move {
			if let Err(err) = conn.run().await {
				tracing::warn!(%err, "connection closed");
			}
		});
	}

	anyhow::bail!("stopped accepting connections")
}
