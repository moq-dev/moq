//! The assembled relay: every startup step, in one place, in the right order.
//!
//! [`Relay::load`] turns a [`Config`] into the running pieces (server, client,
//! auth, cluster, stats, internal + web listeners), and [`Relay::run`] drives
//! them. `main.rs` is a thin wrapper over the two.
//!
//! # Why this is a type and not just `main`
//!
//! An embedder wanting the relay PLUS its own workers - extra routes on the web
//! server, an in-process recorder against [`cluster::Cluster::origin`], another listener
//! in its own `select!` - loads a [`Relay`], clones the handles it needs, mounts
//! routes, and calls [`Relay::run`]. The owner keeps the listeners, worker
//! threads, error propagation, and shutdown joins, so a new socket added here
//! cannot be dropped by a `..` pattern that still compiles.
//!
//! That distinction matters because this crate is consumed as a git dependency.
//! A caller who copies the sequence gets a step added here as a library update
//! with no call site on their end, and nothing says so: the config still parses,
//! nothing errors, and the feature reports as configured while doing nothing.
//! Calling `load` means a new step arrives with the update, and a reshaped API
//! is a compile error.

use anyhow::Context;
use axum::Router;

use crate::{Config, Connection, auth, auth::Admissions, cluster, internal, shutdown, web};

/// A handle that waits until a relay has finished startup.
#[derive(Clone)]
pub struct Ready {
	receiver: tokio::sync::watch::Receiver<bool>,
}

impl Ready {
	/// Wait until `Relay::run` starts serving, or fail if it exits first.
	pub async fn wait(mut self) -> anyhow::Result<()> {
		loop {
			if *self.receiver.borrow() {
				return Ok(());
			}
			self.receiver
				.changed()
				.await
				.context("relay stopped before becoming ready")?;
		}
	}
}

/// A fully assembled relay: the owner of every listener, worker group, and
/// shutdown join.
///
/// The accessors borrow, and [`Self::run`] consumes the relay, so clone the
/// application handles ([`Self::cluster`], [`Self::auth`], [`Self::client`],
/// [`Self::stats`], [`Self::shutdown`], [`Self::shutdown_trigger`]) first,
/// then mount extra routes with [`Self::with_web`] / [`Self::with_internal`].
/// An application that decides admissions itself leaves `[auth]` empty and
/// takes [`Self::admissions`]. `run` is the serving loop; it returns after
/// [`shutdown::Trigger::start`] drains the sessions, with the sockets released
/// and the workers joined.
///
/// ```ignore
/// let relay = Relay::load(config).await?;
/// let origin = relay.cluster().origin.clone();
/// let trigger = relay.shutdown_trigger().clone();
/// let web = relay.web().routes().route("/hello", axum::routing::get(hello));
/// let running = tokio::spawn(relay.with_web(web).run());
/// // ... later, from any task:
/// trigger.start();
/// running.await??;
/// ```
pub struct Relay {
	ready: tokio::sync::watch::Sender<bool>,
	config: Config,
	server: moq_tokio::Listener,
	client: moq_tokio::Client,
	auth: auth::Auth,
	/// The sessions the embedder decides, until it takes them. `None` when the
	/// config named a source; `run` refuses to start while this is still held.
	admissions: Option<Admissions>,
	cluster: cluster::Cluster,
	stats: moq_stats::Producer,
	internal: internal::Internal,
	web: web::Web,
	addr: Option<std::net::SocketAddr>,
	shutdown: shutdown::Observer,
	shutdown_trigger: shutdown::Trigger,
	/// Replacement for the default public router. `None` serves [`web::Web::routes`].
	web_routes: Option<Router>,
	/// Replacement for the default ops router. `None` serves [`internal::Internal::routes`].
	internal_routes: Option<Router>,
	/// Live sessions on this node, listed and nudged from the internal listener.
	sessions: crate::session::Registry,
	/// The thread-per-core QUIC workers, already bound and waiting to be split
	/// and run. `None` unless `runtime.workers` is configured, in which case
	/// `server` carries no QUIC listener of its own.
	#[cfg(feature = "_quic")]
	workers: Option<moq_tokio::worker::Workers>,
	/// The io_uring QUIC workers, already bound and waiting for
	/// [`serve`](crate::uring::Workers::serve). `None` unless both
	/// `runtime.workers` and `runtime.io_uring` are configured, in which case
	/// they own the QUIC listen address instead of `workers`.
	#[cfg(all(target_os = "linux", feature = "_uring"))]
	uring: Option<crate::uring::Workers>,
}

impl Relay {
	/// Assemble a relay from its configuration: bind the listeners, resolve
	/// auth, and build the cluster with its cache and stats attached.
	///
	/// This performs the side effects of starting up (binding every socket,
	/// reading key material, spawning the cache governor), so a returned `Relay`
	/// reports its ephemeral ports and a taken port fails here; no session is
	/// admitted until [`Self::run`] drives it.
	pub async fn load(mut config: Config) -> anyhow::Result<Self> {
		config.resolve()?;
		let resolved_config = config.clone();
		let drain_timeout = config.drain_timeout();
		// The name this relay reports in every auth request: the stats node label,
		// else the cluster node URL, else nothing.
		let node = config
			.stats
			.node
			.clone()
			.or_else(|| config.cluster.node.clone())
			.unwrap_or_default();
		// No `[auth]` source means the embedder decides: it takes the admissions
		// before `run`, which refuses to start if nobody did.
		let (auth, admissions) = match config.auth.is_empty() {
			true => {
				let (auth, admissions) = auth::Auth::embedded(node);
				(auth, Some(admissions))
			}
			false => (config.auth.init(node, &config.connect.tls)?, None),
		};

		#[cfg(feature = "cluster-lan")]
		if config.cluster.lan.enabled {
			cluster::Cluster::validate_lan_versions(&config.connect, &config.listen)?;
		}

		let server_versions = config.listen.versions();

		// Bind the QUIC workers first: they own the listen address when configured,
		// so the server below must not also try to bind it.
		let io_uring = config.runtime.io_uring();
		anyhow::ensure!(
			!io_uring || config.runtime.workers().is_some(),
			"runtime.io_uring requires runtime.workers"
		);
		#[cfg(not(target_os = "linux"))]
		anyhow::ensure!(!io_uring, "runtime.io_uring is Linux-only");
		#[cfg(all(target_os = "linux", not(feature = "_uring")))]
		anyhow::ensure!(
			!io_uring,
			"runtime.io_uring needs moq-relay built with the `io-uring` feature"
		);

		// Refused rather than ignored: a group is the whole point of the setting, and
		// silently serving from the shared runtime instead would look like it worked.
		#[cfg(not(feature = "_quic"))]
		anyhow::ensure!(
			io_uring || config.runtime.workers().is_none(),
			"runtime.workers needs moq-relay built with the `noq` feature"
		);

		#[cfg(feature = "_quic")]
		let workers = match config.runtime.workers() {
			Some(worker) if !io_uring => {
				let mut server = moq_tokio::server::Config::default();
				server.listen = config.listen.clone();
				server.quic = config.quic.clone();
				Some(moq_tokio::worker::Workers::bind(server, worker).context("failed to start the QUIC workers")?)
			}
			_ => None,
		};

		// What the rest of setup reads off the group, so it has one shape whether or
		// not a QUIC backend gave us one to read.
		#[cfg(feature = "_quic")]
		let workers_addr = workers.as_ref().map(moq_tokio::worker::Workers::local_addr);
		#[cfg(not(feature = "_quic"))]
		let workers_addr: Option<std::net::SocketAddr> = None;
		#[cfg(feature = "_quic")]
		let workers_certificates = workers.as_ref().map(moq_tokio::worker::Workers::certificates);
		#[cfg(not(feature = "_quic"))]
		let workers_certificates: Option<moq_tokio::tls::Certificates> = None;

		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let uring = match config.runtime.workers() {
			Some(worker) if io_uring => Some(
				crate::uring::Workers::bind(&config.listen, &config.quic, worker)
					.context("failed to start the io_uring QUIC workers")?,
			),
			_ => None,
		};

		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let quic_owned_elsewhere = workers_addr.is_some() || uring.is_some();
		#[cfg(not(all(target_os = "linux", feature = "_uring")))]
		let quic_owned_elsewhere = workers_addr.is_some();

		#[cfg(feature = "iroh")]
		let iroh = config.iroh.bind(&config.quic).await?;

		#[allow(unused_mut)]
		let mut server_config = moq_tokio::server::Config::default();
		server_config.listen = config.listen.clone();
		server_config.quic = config.quic.clone();
		#[cfg(feature = "iroh")]
		{
			server_config.iroh = iroh.clone();
		}
		let server = match quic_owned_elsewhere {
			true => server_config.init_streams()?,
			false => server_config.init()?,
		};
		let client = config.connect.clone().init(config.quic.clone())?;

		// `None` for a stream-only server (no QUIC); any other error is real.
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let uring_addr = uring.as_ref().map(crate::uring::Workers::local_addr);
		#[cfg(not(all(target_os = "linux", feature = "_uring")))]
		let uring_addr: Option<std::net::SocketAddr> = None;
		let addr = match (workers_addr, uring_addr) {
			(Some(addr), _) => Some(addr),
			(None, Some(addr)) => Some(addr),
			(None, None) => match server.local_addr() {
				Ok(addr) => Some(addr),
				Err(moq_tokio::Error::NoBackend(_)) => None,
				Err(err) => return Err(err).context("failed to resolve the QUIC bind address"),
			},
		};

		#[cfg(feature = "iroh")]
		let client = match iroh {
			Some(iroh) => client.with_iroh(iroh),
			None => client,
		};

		let cache = config.cache.init()?;
		// Whichever worker group owns QUIC holds the certificates; the shared
		// server then has none of its own.
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let uring_certificates = uring.as_ref().map(crate::uring::Workers::certificates);
		#[cfg(not(all(target_os = "linux", feature = "_uring")))]
		let uring_certificates: Option<moq_tokio::tls::Certificates> = None;
		let certificates = match (workers_certificates, uring_certificates) {
			(Some(certificates), _) => certificates,
			(None, Some(certificates)) => certificates,
			(None, None) => server.certificates(),
		};
		let mut advertise = crate::cluster::LanAdvertise::new(addr.map(|a| a.port()).unwrap_or(0));
		let generated = !config.listen.tls.generate.is_empty() || config.listen.tls.identity.is_some();
		if generated && let Some(fingerprint) = certificates.fingerprints().into_iter().next() {
			advertise = advertise.with_fingerprint(fingerprint);
		}
		let cluster = cluster::Cluster::new(cluster::Options::new(config.cluster).with_cache(cache))?
			.with_client(client.clone())
			.with_client_tls(config.connect.tls.build()?)
			.with_connect(config.connect.clone(), config.quic.clone())
			.with_advertise(advertise);
		let stats = config.stats.build(cluster.origin.clone());
		// The cluster takes over keeping the publish task alive, so an embedder that
		// drops the producer keeps publishing for as long as it serves.
		let cluster = cluster.with_stats(stats.clone());

		// Graceful shutdown: the first signal drains every accepted session with a
		// GOAWAY; a second signal (or the drain window elapsing) exits.
		let (shutdown_trigger, shutdown) = shutdown::Observer::new(drain_timeout);
		let sessions = crate::session::Registry::new();
		let (ready, _) = tokio::sync::watch::channel(false);
		let web = web::Web::new(auth.clone(), cluster.clone(), certificates, config.web)
			.with_shutdown(shutdown.clone())
			.with_versions(server_versions)
			.with_sessions(sessions.clone())
			.bind()?;
		// `bind`, not `listen`: the TCP/Unix accept loops handshake as soon as
		// they run, and `load` is not yet willing to take a session. `run`
		// starts them on its first accept.
		let server = server.bind().await.context("failed to bind listeners")?;

		// Internal (ops) listener (plain HTTP, opt-in via `--internal-listen`) for
		// /metrics + /health + /nodes, separate from the customer-facing web server. No-op
		// when unconfigured. Every listener that performs a real accept(2) reports here,
		// web and stream alike: a stream-only relay has no web listener at all, and the
		// point is that whichever socket goes quiet is the one a scrape can see.
		let internal = internal::Internal::new(config.internal, cluster.stats.clone())
			.with_cluster(&cluster)
			.with_sessions(sessions.clone())
			.with_listeners(web.accept_health())
			.with_listeners(server.accept_health())
			.bind()?;
		// Bound but not yet serving: registering here (rather than after the
		// threads start) is what gives every worker a series from the first
		// scrape, including one that is about to fail setup.
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let internal = match uring.as_ref() {
			Some(uring) => internal.with_uring(uring.metrics()),
			None => internal,
		};

		// `kind` so the QUIC line is distinguishable from the web listeners', which
		// log the same way from `web::Web::serve` and may sit on a different port.
		match addr {
			Some(addr) => tracing::info!(%addr, kind = "quic", "listening"),
			None => tracing::info!("listening (stream transports only)"),
		}

		Ok(Relay {
			ready,
			config: resolved_config,
			server,
			client,
			auth,
			admissions,
			cluster,
			stats,
			internal,
			web,
			addr,
			shutdown,
			shutdown_trigger,
			web_routes: None,
			internal_routes: None,
			sessions,
			#[cfg(feature = "_quic")]
			workers,
			#[cfg(all(target_os = "linux", feature = "_uring"))]
			uring,
		})
	}

	/// A handle that waits for [`Self::run`] to finish startup.
	pub fn ready(&self) -> Ready {
		Ready {
			receiver: self.ready.subscribe(),
		}
	}

	/// The resolved configuration used to assemble this relay.
	pub fn config(&self) -> &Config {
		&self.config
	}

	/// The QUIC bind address, or `None` for a stream-only server (no QUIC).
	pub fn addr(&self) -> Option<std::net::SocketAddr> {
		self.addr
	}

	/// The QUIC bind address, or an error for a stream-only relay.
	pub fn quic_addr(&self) -> anyhow::Result<std::net::SocketAddr> {
		self.addr.context("relay has no QUIC listener")
	}

	/// The actual bound HTTP and HTTPS addresses, including ephemeral ports.
	pub fn web_addrs(&self) -> web::Addrs {
		self.web.addrs()
	}

	/// The bound plain TCP (qmux) address, or `None` when `listen.tcp.bind` is unset.
	pub fn tcp_addr(&self) -> Option<std::net::SocketAddr> {
		self.server.tcp_local_addr()
	}

	/// The client used to dial cluster peers. Already handed to [`Self::cluster`];
	/// clone it for your own outbound dials so they share the connection config.
	pub fn client(&self) -> &moq_tokio::Client {
		&self.client
	}

	/// Where every session's grant comes from: the auth server, the static public
	/// grant, or the embedder answering [`Self::admissions`]. Clone it to admit
	/// your own listeners' sessions the same way.
	pub fn auth(&self) -> &auth::Auth {
		&self.auth
	}

	/// The sessions to decide when `[auth]` names no source: take them before
	/// [`Self::run`] and answer each [`Admission`](crate::auth::Admission). `None` when
	/// the config named a source, or once taken.
	pub fn admissions(&mut self) -> Option<Admissions> {
		self.admissions.take()
	}

	/// The shared cluster: the origin every session and peer publishes into.
	pub fn cluster(&self) -> &cluster::Cluster {
		&self.cluster
	}

	/// The stats producer, for publishing extra counters through the same
	/// registry. [`Self::cluster`] holds a clone, so the publish task lives as
	/// long as the cluster whether or not this handle is kept.
	pub fn stats(&self) -> &moq_stats::Producer {
		&self.stats
	}

	/// Graceful-shutdown signal shared by every accepted session and web handler.
	pub fn shutdown(&self) -> &shutdown::Observer {
		&self.shutdown
	}

	/// Starts graceful shutdown: every session drains with a GOAWAY and
	/// [`Self::run`] returns once the drain window elapses. Clone it before
	/// `run` consumes the relay.
	pub fn shutdown_trigger(&self) -> &shutdown::Trigger {
		&self.shutdown_trigger
	}

	/// The customer-facing web surface. Call [`web::Web::routes`] and hand the
	/// result of merging your own routes to [`Self::with_web`].
	pub fn web(&self) -> &web::Web {
		&self.web
	}

	/// The internal (ops) surface: `/metrics`, `/health`, `/nodes`, `/sessions`.
	/// Call [`internal::Internal::routes`] and hand extras to [`Self::with_internal`].
	pub fn internal(&self) -> &internal::Internal {
		&self.internal
	}

	/// Live sessions on this node. Clone it onto an embedder's own listeners so
	/// they register too, and onto any extra ops routes that list or nudge them.
	pub fn sessions(&self) -> &crate::session::Registry {
		&self.sessions
	}

	/// Report embedder-owned listeners at the relay's `/metrics` endpoint.
	#[must_use = "the relay with the extra listeners is returned"]
	pub fn with_listeners(mut self, health: impl IntoIterator<Item = moq_tokio::accept::Health>) -> Self {
		self.internal = self.internal.with_listeners(health);
		self
	}

	/// Serve `routes` on the public HTTP/HTTPS listeners instead of the
	/// relay's default router.
	///
	/// Build `routes` from [`web::Web::routes`] plus whatever the
	/// application nests or merges; this replaces the router, so a bare
	/// `Router::new()` drops every built-in route (health, certificate
	/// fingerprint, announced, fetch, WebSocket). [`Self::run`] still owns the
	/// listeners.
	#[must_use = "the relay with the extra routes is returned"]
	pub fn with_web(mut self, routes: Router) -> Self {
		self.web_routes = Some(routes);
		self
	}

	/// Serve `routes` on the internal (ops) listener instead of the relay's
	/// default ops router.
	///
	/// Build `routes` from [`internal::Internal::routes`] plus extras;
	/// this replaces the router, so a bare `Router::new()` drops `/metrics`,
	/// `/health`, `/nodes`, and `/sessions`. [`Self::run`] still owns the listener.
	#[must_use = "the relay with the extra routes is returned"]
	pub fn with_internal(mut self, routes: Router) -> Self {
		self.internal_routes = Some(routes);
		self
	}

	/// Serve until something fails or shutdown completes: accept sessions, run
	/// the cluster, and serve both HTTP surfaces. Notifies systemd once
	/// everything is up. Returns once the drain window elapses after a signal
	/// or [`shutdown::Trigger::start`], with every listener released and every
	/// worker joined.
	///
	/// This is also the embedding loop. Extra routes go on via [`Self::with_web`]
	/// / [`Self::with_internal`] before calling this; cloned handles outlive it.
	pub async fn run(self) -> anyhow::Result<()> {
		let Relay {
			ready,
			server,
			auth,
			admissions,
			cluster,
			internal,
			web,
			shutdown,
			shutdown_trigger,
			web_routes,
			internal_routes,
			sessions,
			#[cfg(feature = "_quic")]
			workers,
			#[cfg(all(target_os = "linux", feature = "_uring"))]
			uring,
			..
		} = self;

		let web_routes = web_routes.unwrap_or_else(|| web.routes());
		let internal_routes = internal_routes.unwrap_or_else(|| internal.routes());

		// Nobody configured and nobody took over: refuse before binding is
		// reported ready, the way a missing source refuses a binary.
		anyhow::ensure!(
			admissions.is_none(),
			"no --auth-url or --auth-public configured; nobody can authenticate (an embedder decides by taking Relay::admissions)"
		);

		// Validate the cluster and bind its LAN advertisement before claiming to be
		// ready: the `cluster.run()` below is first polled after the notify, so a bad
		// key or an mDNS failure would otherwise release the units depending on a
		// relay that is about to exit.
		let started = cluster.clone().start().await.context("cluster failed to start")?;

		// Before the readiness notify, for the same reason the cluster starts
		// before it: an unsupported kernel, a refused ring, or a certificate
		// certificate identity noq will not load fails here, and reporting ready first would
		// release the units depending on a relay that is about to exit.
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let mut uring = uring;
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		if let Some(uring) = uring.as_mut() {
			uring
				.serve(cluster.clone(), auth.clone(), shutdown.clone(), sessions.clone())
				.context("failed to start the io_uring QUIC workers")?;
		}

		ready.send_replace(true);

		#[cfg(unix)]
		// Notify systemd that we're ready after all initialization is complete
		let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);

		#[cfg(feature = "jemalloc")]
		let jemalloc = moq_tokio::jemalloc::run();
		#[cfg(not(feature = "jemalloc"))]
		let jemalloc = std::future::pending::<anyhow::Result<()>>();

		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let uring_serving = uring.is_some();
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let uring_failed = {
			let uring = uring.as_mut();
			async move {
				match uring {
					Some(uring) => uring.failed().await,
					None => std::future::pending().await,
				}
			}
		};
		#[cfg(not(all(target_os = "linux", feature = "_uring")))]
		let uring_failed = std::future::pending::<anyhow::Error>();

		// Each worker serves from its own thread, so the future built here only
		// reports the outcome. The group owns those threads and ends serving
		// when the first member finishes, so it has to outlive the loop below
		// and is torn down after it.
		#[cfg(feature = "_quic")]
		let mut workers = workers.map(|workers| workers.split());

		// Pends forever with no workers, so it composes into the `select!` either
		// way. A worker only stops on error or on shutdown, so the first one to
		// finish ends the relay the same way the shared accept loop does.
		#[cfg(feature = "_quic")]
		let quic_workers = {
			let mut running = futures::stream::FuturesUnordered::new();
			if let Some(workers) = workers.as_mut() {
				for member in workers.members() {
					let index = member.index();
					let cluster = cluster.clone();
					let auth = auth.clone();
					let worker_shutdown = shutdown.clone();
					let sessions = sessions.clone();
					let task = member.serve(move |server| serve(server, cluster, auth, worker_shutdown, sessions));
					running.push(async move {
						match task.await {
							Ok(res) => res.with_context(|| format!("QUIC worker {index} failed")),
							Err(err) => Err(anyhow::Error::new(err).context(format!("QUIC worker {index} stopped"))),
						}
					});
				}
			}

			async move {
				use futures::StreamExt;
				match running.next().await {
					Some(res) => res,
					None => std::future::pending().await,
				}
			}
		};
		// No group to run, so this leg never resolves and the shared accept loop
		// below decides the `select!` on its own.
		#[cfg(not(feature = "_quic"))]
		let quic_workers = std::future::pending::<anyhow::Result<()>>();

		#[cfg(feature = "_quic")]
		let has_workers = workers.is_some();
		#[cfg(not(feature = "_quic"))]
		let has_workers = false;

		// With the QUIC listener owned by a worker group, the shared server has
		// only the `tcp`/`unix` stream listeners left, and a config with none
		// of those has nothing to accept: its loop would report "stopped
		// accepting" immediately and take the healthy relay down. Pend instead;
		// the workers are what serve.
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let quic_on_workers = has_workers || uring_serving;
		#[cfg(not(all(target_os = "linux", feature = "_uring")))]
		let quic_on_workers = has_workers;
		// Iroh is not an `accept(2)` listener, so it never shows up in the
		// health list; pending on the shared loop would leave its endpoint
		// unpolled and every inbound `iroh://` connection hanging.
		#[cfg(feature = "iroh")]
		let has_iroh = server.iroh_endpoint().is_some();
		#[cfg(not(feature = "iroh"))]
		let has_iroh = false;
		let serve_shared = {
			let idle = quic_on_workers && server.accept_health().is_empty() && !has_iroh;
			let cluster = cluster.clone();
			let auth = auth.clone();
			let shutdown = shutdown.clone();
			let sessions = sessions.clone();
			async move {
				match idle {
					true => std::future::pending().await,
					false => serve_listening(server, cluster, auth, shutdown, sessions).await,
				}
			}
		};

		let result = tokio::select! {
			Err(err) = started.run() => Err(err).context("cluster failed"),
			Err(err) = web.serve(web_routes) => Err(err).context("web server failed"),
			Err(err) = internal.serve(internal_routes) => Err(err).context("internal server failed"),
			Err(err) = serve_shared => Err(err).context("server failed"),
			Err(err) = quic_workers => Err(err).context("QUIC workers failed"),
			err = uring_failed => Err(err).context("io_uring QUIC workers failed"),
			Err(err) = jemalloc => Err(err).context("jemalloc profiler failed"),
			res = drain(shutdown_trigger, shutdown.clone()) => res,
			else => Ok(()),
		};

		// Explicitly, so the joins land on the blocking pool rather than on the
		// executor thread this future happens to be running on.
		#[cfg(feature = "_quic")]
		if let Some(workers) = workers {
			workers.shutdown().await;
		}
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		if let Some(uring) = uring {
			uring.shutdown().await;
		}

		result
	}
}

/// Two-stage shutdown: the first signal, or an embedder firing
/// [`shutdown::Trigger::start`], starts the drain broadcast (every session sends
/// GOAWAY and waits for its peer to leave); a second signal, or the drain
/// window elapsing, returns from [`Relay::run`].
async fn drain(trigger: shutdown::Trigger, mut shutdown: shutdown::Observer) -> anyhow::Result<()> {
	let window = shutdown.drain_timeout;
	tokio::select! {
		res = shutdown_signal() => {
			res?;
			tracing::info!(
				?window,
				"shutdown signal received; draining sessions (signal again to exit immediately)"
			);
			trigger.start();
		}
		_ = shutdown.started() => tracing::info!(?window, "shutdown requested; draining sessions"),
	}

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

/// Accept sessions off `server` until it stops, spawning a [`Connection`] task
/// for each.
///
/// The accept loop for a single [`moq_tokio::Server`]. [`Relay::run`] owns
/// worker selection and shutdown for embedders.
#[cfg(feature = "_quic")]
async fn serve(
	server: moq_tokio::Server,
	cluster: cluster::Cluster,
	auth: auth::Auth,
	shutdown: shutdown::Observer,
	sessions: crate::session::Registry,
) -> anyhow::Result<()> {
	// Each QUIC worker binds here; Relay::run binds the shared listener before
	// readiness and passes it to the same accept loop.
	let listener = server.listen().await.context("failed to bind listeners")?;
	serve_listening(listener, cluster, auth, shutdown, sessions).await
}

async fn serve_listening(
	mut listener: moq_tokio::Listener,
	cluster: cluster::Cluster,
	auth: auth::Auth,
	shutdown: shutdown::Observer,
	sessions: crate::session::Registry,
) -> anyhow::Result<()> {
	while let Some(request) = listener.accept().await {
		let conn = Connection::new(request, cluster.clone(), auth.clone())
			.with_id(cluster.next_connection_id())
			.with_shutdown(shutdown.clone())
			.with_sessions(sessions.clone());

		tokio::spawn(async move {
			if let Err(err) = conn.run().await {
				tracing::warn!(%err, "connection closed");
			}
		});
	}

	anyhow::bail!("stopped accepting connections")
}

#[cfg(all(test, not(feature = "_quic")))]
mod tests {
	#[tokio::test]
	async fn workers_require_a_quic_backend() {
		let mut config = crate::Config::default();
		config.runtime.workers = Some(1);
		let error = match super::Relay::load(config).await {
			Ok(_) => panic!("workers accepted without a QUIC backend"),
			Err(error) => error,
		};
		assert_eq!(
			error.to_string(),
			"runtime.workers needs moq-relay built with the `noq` feature"
		);
	}
}
