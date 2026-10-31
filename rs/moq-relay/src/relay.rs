//! The assembled relay: every startup step, in one place, in the right order.
//!
//! [`Relay::load`] turns a [`Config`] into the running pieces (server, auth,
//! cluster, stats, internal + web listeners), and [`Relay::run`] drives
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
/// application handles ([`Self::cluster`], [`Self::auth`],
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
	auth: auth::Auth,
	/// The sessions the embedder decides, until it takes them. `None` when the
	/// config named a source; `run` refuses to start while this is still held.
	admissions: Option<Admissions>,
	cluster: cluster::Cluster,
	stats: moq_stats::Producer,
	internal: internal::Internal,
	web: web::Web,
	/// The ACME client, issuing before [`Self::run`] serves and renewing after.
	#[cfg(feature = "acme")]
	acme: Option<crate::acme::Acme>,
	#[cfg(feature = "stun")]
	stun: Option<crate::stun::Server>,
	addr: Option<std::net::SocketAddr>,
	shutdown: shutdown::Observer,
	shutdown_trigger: shutdown::Trigger,
	/// Whether [`Self::run`] drains on SIGINT/SIGTERM itself, or leaves that to
	/// the embedder firing [`Self::shutdown_trigger`].
	signals: bool,
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
	/// [`serve`](crate::uring::Spawned::serve). `None` unless both
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
	pub async fn load(config: Config) -> anyhow::Result<Self> {
		// Keep the large debug state machine off the embedder's main-thread stack.
		Box::pin(Self::load_inner(config)).await
	}

	async fn load_inner(mut config: Config) -> anyhow::Result<Self> {
		config.resolve()?;
		// A relay keeps the moq-transport object state it forwards across a moq-lite hop to
		// another relay. Only negotiated when both sides offer it, so other peers never see it.
		config.listen.extensions.ietf = true;
		config.connect.extensions.ietf = true;
		// Name the relay ahead of its core unless the operator overrides it (empty omits it),
		// and dial cluster peers under the same name.
		let implementation = config
			.listen
			.implementation
			.get_or_insert_with(|| format!("moq-relay/{} {}", env!("CARGO_PKG_VERSION"), moq_net::IMPLEMENTATION))
			.clone();
		config.connect.implementation = Some(implementation.clone());
		// Every TLS listener serves the ACME certificate, so install it before any is built.
		#[cfg(feature = "acme")]
		let acme = config.acme.init(&config.listen.tls, &config.web.https)?;
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
			false => {
				let client_ca = !(config.listen.tls.root.is_empty() && config.web.https.root.is_empty());
				(config.auth.init(node, &config.connect.tls, client_ca)?, None)
			}
		};

		#[cfg(feature = "cluster-lan")]
		if config.cluster.lan.enabled {
			cluster::Cluster::validate_lan_versions(&config.connect, &config.listen)?;
		}

		// Every listener shares admission ownership before its workers can accept.
		let (shutdown_trigger, shutdown) = shutdown::Observer::new(drain_timeout);
		let server_versions = config.listen.versions();
		let handshake_timeout = config.listen.resolved_timeout();
		let server_extensions = config.listen.extensions;

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
			"runtime.workers needs moq-relay built with the `quic` feature"
		);

		// Workers and the stream server split one relay across listeners, so they share
		// one loaded identity. A lone server loads its own, only if a listener needs it.
		#[cfg(feature = "acme")]
		let resolved = acme
			.as_ref()
			.map(|acme| moq_tokio::tls::Serve::resolver(acme.resolver(), &config.listen.tls))
			.transpose()?;
		#[cfg(not(feature = "acme"))]
		let resolved = None;
		let served = match resolved {
			Some(served) => Some(served),
			None => config.runtime.workers().map(|_| config.listen.tls.load()).transpose()?,
		};
		let mut listen = config.listen.clone();
		if served.is_some() {
			listen.tls = Default::default();
		}

		#[cfg(feature = "_quic")]
		let workers = match config.runtime.workers() {
			Some(worker) if !io_uring => {
				let mut server = moq_tokio::server::Config::default();
				server.listen = listen.clone();
				server.tls = served.clone();
				server.quic = config.quic.clone();
				server.activity = shutdown.activity.clone();
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
			Some(worker) if io_uring => {
				let mut server = moq_tokio::server::Config::default();
				server.listen = listen.clone();
				server.quic = config.quic.clone();
				server.tls = served.clone();
				Some(crate::uring::Workers::bind(server, worker).context("failed to start the io_uring QUIC workers")?)
			}
			_ => None,
		};

		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let quic_owned_elsewhere = workers_addr.is_some() || uring.is_some();
		#[cfg(not(all(target_os = "linux", feature = "_uring")))]
		let quic_owned_elsewhere = workers_addr.is_some();

		#[cfg(feature = "iroh")]
		let iroh = config.iroh.bind(&config.quic).await?;

		let mut server_config = moq_tokio::server::Config::default();
		server_config.listen = listen;
		server_config.tls = served;
		server_config.quic = config.quic.clone();
		server_config.activity = shutdown.activity.clone();
		#[cfg(feature = "iroh")]
		{
			server_config.iroh = iroh.clone();
		}
		let server = match quic_owned_elsewhere {
			true => server_config.init_streams()?,
			false => server_config.init()?,
		};

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
		let mut cluster_options = cluster::Options::new(config.cluster).with_cache(cache);
		cluster_options.session = config.limits.client;
		cluster_options.peer = config.limits.peer;
		let cluster = cluster::Cluster::new(cluster_options)?
			.with_connect(config.connect.clone(), config.quic.clone())
			.with_advertise(advertise);
		#[cfg(feature = "iroh")]
		let cluster = match iroh {
			Some(iroh) => cluster.with_iroh(iroh),
			None => cluster,
		};
		let stats = config.stats.build(cluster.origin.clone())?;
		// The cluster takes over keeping the publish task alive, so an embedder that
		// drops the producer keeps publishing for as long as it serves.
		let cluster = cluster.with_stats(stats.clone());

		let sessions = crate::session::Registry::new();
		let (ready, _) = tokio::sync::watch::channel(false);
		let web = web::Web::new(auth.clone(), cluster.clone(), certificates, config.web)
			.with_shutdown(shutdown.clone())
			.with_versions(server_versions)
			.with_extensions(server_extensions)
			.with_implementation(implementation)
			.with_timeout(handshake_timeout)
			.with_sessions(sessions.clone());
		#[cfg(feature = "acme")]
		let web = web.with_acme(acme.as_ref().map(crate::acme::Acme::resolver));
		let web = web.bind()?;
		// `bind`, not `listen`: the TCP/Unix accept loops handshake as soon as
		// they run, and `load` is not yet willing to take a session. `run`
		// starts them on its first accept.
		let server = server.bind().await.context("failed to bind listeners")?;

		#[cfg(feature = "stun")]
		let stun = crate::stun::Server::bind(config.stun).await?;

		// Internal (ops) listener (plain HTTP, opt-in via `--internal-listen`) for
		// /metrics + /health + /nodes, separate from the customer-facing web server. No-op
		// when unconfigured. Every listener that performs a real accept(2) reports here,
		// web and stream alike: a stream-only relay has no web listener at all, and the
		// point is that whichever socket goes quiet is the one a scrape can see.
		let internal = internal::Internal::new(config.internal, cluster.stats.clone())
			.with_timeout(handshake_timeout)
			.with_cluster(&cluster)
			.with_sessions(sessions.clone())
			.with_shutdown(shutdown.clone())
			.with_listeners(web.accept_health())
			.with_listeners(server.accept_health())
			.bind()?;
		#[cfg(feature = "stun")]
		let internal = internal.with_stun(stun.as_ref().map(|stun| stun.counters.clone()));
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
			auth,
			admissions,
			cluster,
			stats,
			internal,
			web,
			#[cfg(feature = "acme")]
			acme,
			#[cfg(feature = "stun")]
			stun,
			addr,
			shutdown,
			shutdown_trigger,
			signals: true,
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

	/// Starts graceful shutdown: every session, including any accepted
	/// afterwards, drains with a GOAWAY and [`Self::run`] returns once every
	/// session has left or the drain window elapses. Clone it before `run`
	/// consumes the relay.
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

	/// Whether [`Self::run`] starts the drain on SIGINT/SIGTERM. Defaults to
	/// `true`; pass `false` when the application owns the signals and fires
	/// [`Self::shutdown_trigger`] itself, e.g. after withdrawing the node from DNS.
	///
	/// `run` installs the handlers just before it serves, once its startup can no
	/// longer fail, so a signal before that takes its default action. With ACME
	/// it installs them before the first issuance instead, so a stop request
	/// there still ends `run`. They stay installed for the life of the process,
	/// since tokio never removes one, even after `run` fails.
	#[must_use = "the relay with the signal choice is returned"]
	pub fn with_signals(mut self, signals: bool) -> Self {
		self.signals = signals;
		self
	}

	/// Serve until something fails or shutdown completes: accept sessions, run
	/// the cluster, and serve both HTTP surfaces. Notifies systemd once
	/// everything is up. Returns once a drain started by a signal (see
	/// [`Self::with_signals`]) or [`shutdown::Trigger::start`] ends, as soon as
	/// every session has left or at the drain deadline, with every listener
	/// released and every worker joined.
	///
	/// This is also the embedding loop. Extra routes go on via [`Self::with_web`]
	/// / [`Self::with_internal`] before calling this; cloned handles outlive it.
	pub async fn run(self) -> anyhow::Result<()> {
		// Serving branches must not inflate the caller's stack frame.
		Box::pin(self.run_inner()).await
	}

	async fn run_inner(self) -> anyhow::Result<()> {
		let Relay {
			ready,
			mut server,
			auth,
			admissions,
			cluster,
			internal,
			web,
			#[cfg(feature = "acme")]
			acme,
			#[cfg(feature = "stun")]
			stun,
			shutdown,
			shutdown_trigger,
			signals,
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

		let mut handlers = None;

		// Nothing else serves or reports ready until the certificate exists: a
		// self-signed placeholder would only fail every browser that connects.
		#[cfg(feature = "acme")]
		if let Some(mut acme) = acme {
			let listener = web.https_listener().context("ACME needs the HTTPS listener")?;
			// Issuance can take minutes, and as a container's PID 1 the relay
			// ignores a signal left at its default action, so a stop would wait it
			// out. That outweighs leaving the handlers installed if a later step fails.
			if signals {
				handlers = Some(shutdown::Signals::new()?);
			}
			tokio::select! {
				res = acme.issue(listener) => res?,
				// No session exists yet, so this returns as soon as it starts.
				() = drain(shutdown_trigger.clone(), shutdown.clone(), handlers.as_mut()) => return Ok(()),
			}
			cluster.critical.spawn("ACME renewal", acme.run());
		}

		// Validate the cluster and bind its LAN advertisement before claiming to be
		// ready: the `cluster.run()` below is first polled after the notify, so a bad
		// key or an mDNS failure would otherwise release the units depending on a
		// relay that is about to exit.
		let started = cluster.clone().start().await.context("cluster failed to start")?;

		// Before the readiness notify, for the same reason the cluster starts
		// before it: an unsupported kernel, a refused ring, or a certificate
		// identity rustls will not load fails here, and reporting ready first would
		// release the units depending on a relay that is about to exit.
		#[cfg(all(target_os = "linux", feature = "_uring"))]
		let mut uring = uring
			.map(|uring| {
				uring
					.spawn(cluster.clone(), auth.clone(), shutdown.clone(), sessions.clone())
					.context("failed to start the io_uring QUIC workers")
			})
			.transpose()?;

		// Without ACME, after the last step that can fail, since a handler outlives a
		// failed `run`, and before anything accepts a session a signal would kill
		// without a GOAWAY.
		// Nothing awaits from here to the drain below, so a signal caught in between
		// starts the drain at its first poll, as one just after READY would.
		if signals && handlers.is_none() {
			handlers = Some(shutdown::Signals::new()?);
		}

		#[cfg(all(target_os = "linux", feature = "_uring"))]
		if let Some(uring) = uring.as_mut() {
			uring.serve();
		}

		// An embedder that already stopped the relay gets a drain, not a READY.
		if shutdown.deadline().is_none() {
			ready.send_replace(true);

			#[cfg(unix)]
			// Notify systemd that we're ready after all initialization is complete
			let _ = sd_notify::notify(&[sd_notify::NotifyState::Ready]);
		}

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
			let server = &mut server;
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

		let stun = async move {
			#[cfg(feature = "stun")]
			match stun {
				Some(stun) => stun.run().await,
				None => std::future::pending().await,
			}
			#[cfg(not(feature = "stun"))]
			std::future::pending::<anyhow::Result<()>>().await
		};
		let result = tokio::select! {
			Err(err) = started.run() => Err(err).context("cluster failed"),
			Err(err) = web.serve(web_routes) => Err(err).context("web server failed"),
			Err(err) = stun => Err(err).context("STUN server failed"),
			Err(err) = internal.serve(internal_routes) => Err(err).context("internal server failed"),
			Err(err) = serve_shared => Err(err).context("server failed"),
			Err(err) = quic_workers => Err(err).context("QUIC workers failed"),
			err = uring_failed => Err(err).context("io_uring QUIC workers failed"),
			Err(err) = jemalloc => Err(err).context("jemalloc profiler failed"),
			() = drain(shutdown_trigger, shutdown.clone(), handlers.as_mut()) => Ok(()),
			else => Ok(()),
		};

		// Before the runtime goes, so each dialed peer drops our routes at once.
		cluster.close().await;

		// Dropping the listener would leave its QUIC socket open until the
		// endpoint's closing connections finish in the background, past `run`.
		server.close().await;

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
/// GOAWAY and waits for its peer to leave). Returns from [`Relay::run`] once
/// [`shutdown::Observer::drained`] does, or on a second signal.
/// Without `signals` only the trigger and the sessions count.
async fn drain(
	trigger: shutdown::Trigger,
	mut shutdown: shutdown::Observer,
	mut signals: Option<&mut shutdown::Signals>,
) {
	let window = shutdown.drain_timeout;
	let mut signal = async || match signals.as_mut() {
		Some(signals) => signals.recv().await,
		None => std::future::pending().await,
	};
	tokio::select! {
		// A drain the embedder already started leaves a queued signal to be the
		// second request, not another first one.
		biased;
		() = shutdown.started() => tracing::info!(?window, "shutdown requested; draining sessions"),
		() = signal() => {
			tracing::info!(
				?window,
				"shutdown signal received; draining sessions (signal again to exit immediately)"
			);
			trigger.start();
		}
	}

	tokio::select! {
		() = signal() => {
			tracing::warn!(open = shutdown.tally().live, "second shutdown signal; exiting immediately");
		}
		() = shutdown.drained() => {}
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
	let mut listener = server.listen().await.context("failed to bind listeners")?;
	serve_listening(&mut listener, cluster, auth, shutdown, sessions).await
}

async fn serve_listening(
	listener: &mut moq_tokio::Listener,
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
		tokio::spawn(conn.run());
	}

	anyhow::bail!("stopped accepting connections")
}

#[cfg(test)]
mod tests {
	#[cfg(feature = "stun")]
	#[tokio::test]
	async fn stun_listener_runs_with_the_relay_and_stops_with_it() {
		use stun_types::message::{BINDING, Message, MessageWriteVec};
		use stun_types::prelude::*;
		let mut config = crate::Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		config.stun.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut relay = super::Relay::load(config).await.unwrap().with_signals(false);
		let _admissions = relay.admissions().unwrap();
		let addr = relay.stun.as_ref().unwrap().addr();
		let ready = relay.ready();
		let shutdown = relay.shutdown_trigger().clone();
		let running = tokio::spawn(relay.run());
		ready.wait().await.unwrap();
		let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
		let request = Message::builder_request(BINDING, MessageWriteVec::new()).finish();
		client.send_to(&request, addr).await.unwrap();
		let mut reply = [0; 44];
		let (len, sender) = moq_clock::timeout(std::time::Duration::from_secs(5), client.recv_from(&mut reply))
			.await
			.unwrap()
			.unwrap();
		assert_eq!(sender, addr);
		assert_eq!(len, 32);
		shutdown.start();
		running.await.unwrap().unwrap();
	}

	#[tokio::test(start_paused = true)]
	async fn pending_handshake_cannot_extend_the_drain_deadline() {
		use futures::FutureExt;
		let window = std::time::Duration::from_secs(10);
		let (trigger, shutdown) = crate::shutdown::Observer::new(window);
		let _pending = shutdown.activity.enter();
		trigger.start();
		let draining = super::drain(trigger, shutdown, None);
		tokio::pin!(draining);
		assert!(draining.as_mut().now_or_never().is_none());
		tokio::time::advance(window).await;
		assert!(
			draining.as_mut().now_or_never().is_some(),
			"pending handshake extended the fixed deadline"
		);
	}

	/// A signal queued behind an embedder's trigger is the second request, so it
	/// exits at once rather than starting the drain again.
	#[cfg(unix)]
	#[tokio::test(start_paused = true)]
	async fn a_signal_after_an_early_trigger_exits_at_once() {
		let _signals = crate::shutdown::SIGNALS.lock().await;
		// Either arm is ready, so an unbiased wait would pass some runs.
		for _ in 0..32 {
			let (trigger, shutdown) = crate::shutdown::Observer::new(std::time::Duration::from_secs(10));
			let _active = shutdown.activity.enter();
			trigger.start();
			let mut signals = crate::shutdown::Signals::new().unwrap();
			crate::shutdown::raise(libc::SIGTERM).await;
			let started = moq_clock::now();
			super::drain(trigger, shutdown, Some(&mut signals)).await;
			assert_eq!(started.elapsed(), std::time::Duration::ZERO, "waited out the drain");
		}
	}

	/// A relay stopped before it finished starting drains without reporting ready.
	#[tokio::test]
	async fn a_stop_during_startup_skips_readiness() {
		let mut config = crate::Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut relay = super::Relay::load(config).await.unwrap().with_signals(false);
		let _admissions = relay.admissions().unwrap();
		let ready = relay.ready();
		relay.shutdown_trigger().start();
		relay.run().await.unwrap();
		assert!(ready.wait().await.is_err(), "reported ready after the stop");
	}

	/// A connection arriving as the last admitted one leaves is either counted
	/// before the drain commits to exit, or refused at accept; never admitted
	/// into a relay that has already decided to exit under it.
	#[tokio::test(start_paused = true)]
	async fn arrival_as_the_drain_empties_is_counted_or_refused() {
		use futures::FutureExt;
		let (trigger, shutdown) = crate::shutdown::Observer::new(std::time::Duration::from_secs(10));
		let activity = shutdown.activity.clone();
		let last = activity.enter().expect("admission open before the drain");
		trigger.start();
		let draining = super::drain(trigger, shutdown, None);
		tokio::pin!(draining);
		assert!(draining.as_mut().now_or_never().is_none());

		// Arrives before the drain observes the count reach zero: it is counted.
		drop(last);
		let arrival = activity.enter().expect("admitted before the drain committed");
		assert!(
			draining.as_mut().now_or_never().is_none(),
			"drain exited under an admitted arrival"
		);

		// Arrives just after the count reaches zero: refused at accept.
		drop(arrival);
		assert!(draining.as_mut().now_or_never().is_some());
		assert!(
			activity.enter().is_none(),
			"admitted a connection after the drain committed to exit"
		);
	}

	/// A long-lived task's panic ends `run` with that panic, instead of leaving a
	/// relay that accepts sessions it can no longer serve.
	#[tokio::test]
	async fn a_critical_panic_stops_the_relay() {
		let mut config = crate::Config::default();
		config.listen.tcp.bind = Some("127.0.0.1:0".parse().unwrap());
		let mut relay = super::Relay::load(config).await.unwrap().with_signals(false);
		let _admissions = relay.admissions().unwrap();
		relay
			.cluster()
			.critical
			.spawn("injected task", async { panic!("injected") });

		let err = relay.run().await.expect_err("the relay kept running");
		assert_eq!(err.root_cause().to_string(), "injected task panicked: injected");
	}

	#[cfg(not(feature = "_quic"))]
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
			"runtime.workers needs moq-relay built with the `quic` feature"
		);
	}
}
