//! Serving QUIC from io_uring workers.
//!
//! The [`moq_uring`] counterpart of [`moq_tokio::worker`]: one pinned thread
//! per worker, each with its own ring, its own `SO_REUSEPORT` socket steered
//! by connection id ([`moq_sock::shard`]), and its own
//! [`moq_uring::quic::Endpoint`] serving browsers (WebTransport over `h3`)
//! and native peers (raw QUIC) alike, on every version the listener is
//! configured for. moq-lite on its own ALPN runs unboxed on the worker;
//! moq-transport and the SETUP-negotiated versions run through
//! [`moq_uring::transport::Local`], since their drivers need `Send`.
//!
//! Each accepted session runs through the same [`Connection`] as the tokio
//! listeners. The split of work mirrors the relay's topology: a worker owns
//! everything transport-shaped (the handshake, the session's protocol machine),
//! while authentication and session supervision run on the shared tokio runtime,
//! which owns the HTTP client and the origins. The session handle is
//! `Send + Sync` whatever drives its transport, which is what makes the handoff
//! free.

use std::net::SocketAddr;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::task::Poll;

use anyhow::Context as _;
use futures::FutureExt as _;
use tracing::Instrument;

use crate::{Connection, auth, cluster, shutdown};

/// A stop signal a worker parks on, wakeable from the shared runtime.
#[derive(Default)]
struct Stop {
	stopped: AtomicBool,
	waker: Mutex<Option<std::task::Waker>>,
}

impl Stop {
	async fn wait(self: Arc<Self>) {
		std::future::poll_fn(|cx| {
			if self.stopped.load(Ordering::Acquire) {
				return Poll::Ready(());
			}
			*self.waker.lock().unwrap_or_else(PoisonError::into_inner) = Some(cx.waker().clone());
			if self.stopped.load(Ordering::Acquire) {
				return Poll::Ready(());
			}
			Poll::Pending
		})
		.await
	}

	fn stop(&self) {
		self.stopped.store(true, Ordering::Release);
		if let Some(waker) = self.waker.lock().unwrap_or_else(PoisonError::into_inner).take() {
			waker.wake();
		}
	}
}

/// Everything a worker needs to serve a connection, cloned per thread.
#[derive(Clone)]
struct Serve {
	cluster: cluster::Cluster,
	auth: auth::Auth,
	shutdown: shutdown::Observer,
	sessions: crate::session::Registry,
	/// The shared runtime, which owns authentication (the auth API's HTTP
	/// client needs its reactor) and session supervision.
	tokio: tokio::runtime::Handle,
	/// The moq ALPNs this listener speaks, for the WebTransport subprotocol
	/// pick.
	alpns: Arc<Vec<String>>,
	/// The moq handshake every connection runs, mapped from `listen` as the
	/// tokio listeners map it, opening `[quic] receive_window` on admission.
	moq: moq_net::Server,
	/// The address every worker socket shares, reported as a session's `local`.
	local: SocketAddr,
	/// `listen.timeout`, which each worker's endpoint starts at a connection's
	/// first packet.
	timeout: Option<std::time::Duration>,
	/// `listen.reset.secret`, shared so any worker resets any worker's
	/// connections, a restarted group's included.
	reset_secret: Option<[u8; 32]>,
	/// The served TLS policy, which builds each client's identity against its roots.
	tls: moq_tokio::tls::Serve,
}

/// A bound group of io_uring QUIC workers sharing one port.
///
/// [`bind`](Self::bind) opens every socket, so a returned value already owns
/// the listen address and a port conflict is an error here rather than a
/// worker that quietly died. [`spawn`](Self::spawn) starts every thread, and
/// nothing is served until [`Spawned::serve`].
pub struct Workers {
	members: Vec<moq_sock::shard::Socket>,
	addr: SocketAddr,
	server: moq_uring::quic::server::Config,
	/// The TLS policy every worker serves, and the live resolver behind the
	/// fingerprint endpoint.
	tls: moq_tokio::tls::Serve,
	/// One counter set per worker, in shard order. Created here rather than on
	/// the worker thread so `/metrics` can publish every worker's series from
	/// the first scrape, before the threads exist: a series that only appears
	/// once a worker has done something reads as a healthy node until it is too
	/// late to notice.
	metrics: Vec<moq_uring::metrics::Metrics>,
	udp: moq_uring::udp::Config,
	pin: bool,
	alpns: Arc<Vec<String>>,
	moq: moq_net::Server,
	timeout: Option<std::time::Duration>,
	reset_secret: Option<[u8; 32]>,

	threads: Vec<(u16, std::thread::JoinHandle<()>)>,
	stops: Vec<Arc<Stop>>,
	/// Releases each spawned worker to accept; dropped unsent, it exits instead.
	starts: Vec<std::sync::mpsc::Sender<()>>,
	/// A worker's fatal error, reported to whoever polls [`Spawned::failed`].
	failures: tokio::sync::mpsc::UnboundedReceiver<anyhow::Error>,
	failures_tx: tokio::sync::mpsc::UnboundedSender<anyhow::Error>,

	/// The reuseport group the workers bound into. Held for the group's
	/// lifetime, because it is what holds the listen port against a second
	/// group.
	_group: moq_sock::shard::Bound,
}

impl Workers {
	/// Bind one socket per worker on the address `listen` describes.
	///
	/// Shares one served identity with every member; stream listeners are served separately.
	pub fn bind(server: moq_tokio::server::Config, config: moq_tokio::worker::Config) -> anyhow::Result<Self> {
		// Before any TLS loads or socket binds, so a bad `[quic]` has no side effects.
		server.quic.validate()?;
		// Every setting by name, so one added to `listen` fails to compile here until
		// these workers honor or refuse it.
		let moq_tokio::listen::Resolved {
			bind,
			versions,
			// With the versions and timeout, mapped onto the moq handshake by `listen.server()`.
			extensions: _,
			implementation: _,
			timeout,
			tls: listen_tls,
			preferred_v4,
			preferred_v6,
			lb:
				moq_tokio::listen::LoadBalancer {
					id: lb_id,
					nonce: _,
					// Needs an id, refused below.
					forward: _,
				},
			reset_secret,
		} = server.listen.resolve()?;
		// The one mapping of `listen` onto the moq handshake, shared with the tokio listeners.
		let moq = server.listen.server();
		let quic = &server.quic;
		for (configured, setting) in [
			(preferred_v4.is_some(), "listen.preferred_v4"),
			(preferred_v6.is_some(), "listen.preferred_v6"),
		] {
			anyhow::ensure!(
				!configured,
				"io_uring workers cannot advertise a QUIC preferred address ({setting}); use the tokio runtime"
			);
		}
		if lb_id.is_some() {
			anyhow::bail!(
				"io_uring workers issue shard-steered connection ids and cannot also carry a \
				 QUIC-LB server id (listen.lb.id)"
			);
		}
		let serve = match server.tls {
			Some(tls) => {
				// A second configured source must not be silently ignored.
				anyhow::ensure!(
					listen_tls.cert.is_empty()
						&& listen_tls.key.is_empty()
						&& listen_tls.generate.is_empty()
						&& listen_tls.root.is_empty()
						&& listen_tls.identity.is_none()
						&& listen_tls.peers.is_none(),
					"a shared TLS policy cannot be combined with listen.tls settings"
				);
				tls
			}
			None => listen_tls.load()?,
		};

		// One resolution for the whole group, so a DNS answer that rotates
		// between queries cannot hand members different addresses.
		//
		// Named rather than defaulted: an unset `bind` means stream-only when
		// a tcp/unix listener is configured, and defaulting to the usual
		// address here would open a QUIC listener nobody asked for.
		let requested: SocketAddr = {
			use std::net::ToSocketAddrs;
			let bind = bind.as_ref().context("io_uring workers need an explicit listen.bind")?;
			let bind = bind.to_string();
			bind.to_socket_addrs()
				.with_context(|| format!("failed to resolve {bind}"))?
				.next()
				.with_context(|| format!("{bind} resolved to no address"))?
		};

		// The group owns the reuseport invariants: it locks the port against a
		// concurrently-constructing same-UID group (the bind probe covers one
		// that already holds it), refuses a size the steering filter could not
		// address, and binds members in the order the kernel numbers them by.
		let mut group = moq_sock::shard::bind(requested, config.count)
			.with_context(|| format!("failed to bind the reuseport group on {requested}"))?;
		let count = group.count();
		let mut members = Vec::with_capacity(count as usize);
		while let Some(member) = group.member().context("failed to clone a reuseport member")? {
			members.push(member);
		}
		// The requested address, with an ephemeral port resolved.
		let addr = group.addr();

		// Every configured version's ALPN, as the tokio listener advertises.
		// `h3` rides along for browsers, which negotiate the moq version as a
		// WebTransport subprotocol instead.
		let alpns: Vec<String> = versions.alpns().into_iter().map(str::to_owned).collect();

		let alpn = alpns
			.iter()
			.map(|alpn| alpn.as_bytes().to_vec())
			.chain([b"h3".to_vec()])
			.collect();
		let quic = quic.resolve();
		let mut tls = serve.config(alpn)?;
		if let Some(path) = &quic.keylog {
			let tls = Arc::make_mut(&mut tls);
			tls.key_log = Arc::new(moq_tokio::tls::KeyLog::open(path)?);
		}
		let mut server = moq_uring::quic::server::Config::new(tls);
		server.transport = transport(&quic)?;
		// Held back until admission, which opens the configured window.
		let moq = match server.transport.limits.receive_window {
			Some(window) => moq.with_receive_window(window),
			None => moq,
		};
		server.transport.limits.receive_window = NonZeroU64::new(moq_net::transport::SETUP_RECEIVE_WINDOW);

		// GSO is a property of the socket, not the connection, so it rides the
		// worker's UDP config rather than the connection's.
		let mut udp = moq_uring::udp::Config::default();
		udp.gso = quic.gso.unwrap_or(true);

		let reset_secret = reset_secret
			.as_deref()
			.map(moq_tokio::secret::Secret::read)
			.transpose()
			.context("invalid --listen-reset-secret")?
			.map(|secret| *secret.as_bytes());

		let (failures_tx, failures) = tokio::sync::mpsc::unbounded_channel();
		tracing::info!(workers = count, %addr, "bound io_uring QUIC workers");

		Ok(Self {
			metrics: (0..count).map(|_| moq_uring::metrics::Metrics::default()).collect(),
			members,
			addr,
			server,
			tls: serve,
			udp,
			pin: config.pin,
			alpns: Arc::new(alpns),
			moq,
			timeout,
			reset_secret,
			threads: Vec::new(),
			stops: Vec::new(),
			starts: Vec::new(),
			failures,
			failures_tx,
			_group: group,
		})
	}

	/// The address every worker is bound to.
	pub fn local_addr(&self) -> SocketAddr {
		self.addr
	}

	/// Every worker's counters, in shard order, for an ops surface to scrape.
	///
	/// Available from [`bind`](Self::bind), so a scrape covers a worker that has
	/// not started (or has died) instead of dropping its series.
	pub fn metrics(&self) -> Vec<moq_uring::metrics::Metrics> {
		self.metrics.clone()
	}

	/// The certificate every worker presents, for publishing its SHA-256
	/// fingerprint.
	///
	/// Tracks rotations: every worker resolves through the same live identity.
	pub fn certificates(&self) -> moq_tokio::tls::Certificates {
		self.tls.certificates()
	}

	/// Spawn the worker threads, each set up but not yet accepting.
	///
	/// Call from the shared runtime: it is captured as the home for
	/// authentication and session supervision. Returns once every worker is
	/// set up; a worker that cannot start (an old kernel, a ring failure) is
	/// an error here rather than a thread that quietly died. Nothing accepts
	/// until [`Spawned::serve`], so a failure here has served nobody.
	pub fn spawn(
		mut self,
		cluster: cluster::Cluster,
		auth: auth::Auth,
		shutdown: shutdown::Observer,
		sessions: crate::session::Registry,
	) -> anyhow::Result<Spawned> {
		let serve = Serve {
			cluster,
			auth,
			shutdown,
			sessions,
			tokio: tokio::runtime::Handle::current(),
			alpns: self.alpns.clone(),
			moq: self.moq.clone(),
			local: self.addr,
			timeout: self.timeout,
			reset_secret: self.reset_secret,
			tls: self.tls.clone(),
		};

		let cores = match self.pin {
			true => moq_sock::cpu::cores(),
			false => Vec::new(),
		};

		for member in std::mem::take(&mut self.members) {
			let index = member.shard().index();
			let core = cores.get(index as usize % cores.len().max(1)).copied();
			let stop = Arc::new(Stop::default());
			let (ready_tx, ready_rx) = std::sync::mpsc::channel();
			let (start_tx, start_rx) = std::sync::mpsc::channel();

			let spawn = Spawn {
				member,
				core,
				metrics: self.metrics[index as usize].clone(),
				server: self.server.clone(),
				udp: self.udp.clone(),
				serve: serve.clone(),
				stop: stop.clone(),
				ready: ready_tx,
				start: start_rx,
				failures: self.failures_tx.clone(),
			};
			// A scoped embedder subscriber belongs to these workers too; threads do
			// not inherit their creator's tracing dispatcher.
			let dispatch = tracing::dispatcher::get_default(Clone::clone);
			let parent = tracing::Span::current();
			let thread = std::thread::Builder::new()
				.name(format!("moq-uring-{index}"))
				.spawn(move || tracing::dispatcher::with_default(&dispatch, || parent.in_scope(|| run_worker(spawn))))
				.with_context(|| format!("failed to spawn io_uring worker {index}"))?;

			// A worker that fails setup drops its sender, so a recv error and
			// a setup error are the same event.
			match ready_rx.recv() {
				Ok(Ok(())) => {}
				Ok(Err(err)) => {
					let _ = thread.join();
					return Err(err.context(format!("io_uring worker {index} failed to start")));
				}
				Err(_) => {
					let _ = thread.join();
					anyhow::bail!("io_uring worker {index} exited before starting");
				}
			}

			self.threads.push((index, thread));
			self.stops.push(stop);
			self.starts.push(start_tx);
		}

		Ok(Spawned { workers: self })
	}

	/// Start every [`Workers::spawn`]ed worker accepting.
	fn serve(&mut self) {
		for start in self.starts.drain(..) {
			// A worker that died since setup is reported through `failed`.
			let _ = start.send(());
		}
	}

	/// The first worker's fatal error. Pends until one dies, which a healthy
	/// group never does, so this composes into a `select!` loop.
	async fn failed(&mut self) -> anyhow::Error {
		match self.failures.recv().await {
			Some(err) => err,
			// Senders live in `self`, so this is unreachable; pend regardless.
			None => std::future::pending().await,
		}
	}

	/// Stop every worker and wait for its thread, off the caller's runtime.
	async fn shutdown(mut self) {
		let threads = self.signal();
		if threads.is_empty() {
			return;
		}
		let _ = tokio::task::spawn_blocking(move || join(threads)).await;
	}

	fn signal(&mut self) -> Vec<(u16, std::thread::JoinHandle<()>)> {
		// A worker still waiting to serve exits without serving.
		self.starts.clear();
		for stop in &self.stops {
			stop.stop();
		}
		std::mem::take(&mut self.threads)
	}
}

/// Workers that have all initialized and can now begin accepting sessions.
pub struct Spawned {
	workers: Workers,
}

impl Spawned {
	/// Release every initialized worker to accept sessions.
	pub fn serve(&mut self) {
		self.workers.serve();
	}
	/// Wait for the first worker's fatal error.
	pub async fn failed(&mut self) -> anyhow::Error {
		self.workers.failed().await
	}
	/// Stop every worker and wait until its thread has ended.
	pub async fn shutdown(self) {
		self.workers.shutdown().await;
	}
}

impl Drop for Workers {
	fn drop(&mut self) {
		join(self.signal());
	}
}

impl std::fmt::Debug for Workers {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Workers")
			.field("addr", &self.addr)
			.field("threads", &self.threads.len())
			.finish()
	}
}

/// Map the relay's `--quic-*` section onto the worker's transport settings.
///
/// Everything the io_uring stack can express is applied; what it cannot is an
/// error here rather than a setting the operator believes is in force. GSO is
/// the exception in the other direction: it belongs to the socket, so the
/// caller applies it to the UDP config instead.
///
/// One qlog sink is built for the whole group and cloned into every worker, so
/// the traces share a single writer thread however many workers there are.
fn transport(quic: &moq_tokio::quic::Resolved) -> anyhow::Result<moq_uring::quic::transport::Config> {
	// The tokio path reports this through `moq_tokio::Error::QlogUnsupported`,
	// which nothing builds here: this listener is the io_uring stack.
	#[cfg(not(feature = "qlog"))]
	anyhow::ensure!(
		quic.qlog.is_none(),
		"qlog capture requires a build with the 'qlog' feature; drop quic.qlog"
	);

	// Mirrors `moq_tokio::Error::ReceiveWindowTooSmall`: incomplete messages hold
	// connection credit, so a window that cannot fit the largest one stalls.
	anyhow::ensure!(
		quic.receive_window >= moq_net::transport::MIN_RECEIVE_WINDOW,
		"quic.receive_window {} cannot fit the largest message, {} bytes",
		quic.receive_window,
		moq_net::transport::MIN_RECEIVE_WINDOW
	);

	let mut transport = moq_uring::quic::transport::Config::default();
	#[cfg(feature = "qlog")]
	if let Some(dir) = quic.qlog.as_deref() {
		transport.qlog = Some(
			moq_uring::quic::qlog::Sink::directory(dir)
				.with_context(|| format!("failed to capture qlog into {}", dir.display()))?,
		);
		tracing::info!(dir = %dir.display(), "writing qlog");
	}
	// The uring transport spells "no idle timeout" as zero.
	transport.idle_timeout = quic.idle_timeout.unwrap_or_default();
	transport.keep_alive = quic.keep_alive;
	transport.mtu_discovery = quic.mtu_discovery;
	// `[quic]` resolves every limit, defaults included, so both runtimes run
	// the same ones rather than each backend's own.
	transport.limits.max_streams = NonZeroU64::new(quic.max_streams);
	transport.limits.receive_window = NonZeroU64::new(quic.receive_window);
	transport.limits.stream_receive_window = NonZeroU64::new(quic.stream_receive_window);
	transport.limits.send_window = NonZeroU64::new(quic.send_window);
	transport.congestion = match (&quic.controller, quic.congestion_control) {
		// The application's own controller wins over a family, as on the tokio path.
		(Some(controller), _) => controller.0.clone(),
		(None, Some(moq_tokio::quic::CongestionControl::Loss)) => moq_uring::quic::Congestion::Loss.into(),
		// Unset means the backend's own default, and live media wants a steady
		// send rate an encoder can track over CUBIC's sawtooth.
		(None, Some(moq_tokio::quic::CongestionControl::Delay) | None) => moq_uring::quic::Congestion::Delay.into(),
		(None, Some(moq_tokio::quic::CongestionControl::None)) => moq_uring::quic::Congestion::None.into(),
		// A family added to the tokio stack later, rather than a silent
		// substitution the operator would never learn about.
		(None, Some(other)) => anyhow::bail!("io_uring workers cannot run the {other:?} congestion controller"),
	};
	Ok(transport)
}

/// Wait for every worker thread, reporting the ones that panicked.
fn join(threads: Vec<(u16, std::thread::JoinHandle<()>)>) {
	for (index, thread) in threads {
		if thread.join().is_err() {
			tracing::error!(index, "io_uring QUIC worker panicked");
		}
	}
}

/// Everything one worker thread is handed at spawn.
struct Spawn {
	member: moq_sock::shard::Socket,
	/// The core to pin to, or `None` when pinning is off or unavailable.
	core: Option<moq_sock::cpu::CoreId>,
	/// This worker's counter set, shared with whoever scrapes `/metrics`.
	metrics: moq_uring::metrics::Metrics,
	server: moq_uring::quic::server::Config,
	udp: moq_uring::udp::Config,
	serve: Serve,
	stop: Arc<Stop>,
	/// Reports setup back to [`Workers::spawn`], which blocks on it so a
	/// worker that cannot start is an error there rather than a dead thread.
	ready: std::sync::mpsc::Sender<anyhow::Result<()>>,
	/// Fires on [`Spawned::serve`]; closes when the group stops first.
	start: std::sync::mpsc::Receiver<()>,
	/// Where a fatal error goes once the worker is running.
	failures: tokio::sync::mpsc::UnboundedSender<anyhow::Error>,
}

/// One worker thread, with its exit reported however it happens.
///
/// A worker that dies takes its socket out of the steered group while the
/// others keep looking healthy, and the group's filter goes on sending that
/// slot's share of the traffic to a socket nobody is reading. So any exit
/// that was not asked for is fatal to the group, a panic included: `failures`
/// alone would not notice one, since the parent holds a sender of its own and
/// the channel therefore never closes.
fn run_worker(spawn: Spawn) {
	let index = spawn.member.shard().index();
	let stop = spawn.stop.clone();
	let failures = spawn.failures.clone();

	let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| serve_worker(spawn)));
	let asked = stop.stopped.load(Ordering::Acquire);
	match outcome {
		Err(_) => {
			let _ = failures.send(anyhow::anyhow!("io_uring worker {index} panicked"));
		}
		// A worker that started serving only returns when `stop` fires.
		Ok(true) if !asked => {
			let _ = failures.send(anyhow::anyhow!("io_uring worker {index} stopped on its own"));
		}
		// Setup failure already went back through `ready`, which is what
		// `Workers::spawn` is blocking on, and a worker stopped before it
		// served was asked to.
		Ok(_) => {}
	}
}

/// Pin, ring up, and serve the endpoint until stopped. Returns whether the
/// worker ever got as far as serving.
fn serve_worker(spawn: Spawn) -> bool {
	let Spawn {
		member,
		core,
		metrics,
		server,
		udp,
		serve,
		stop,
		ready,
		start,
		failures,
	} = spawn;
	let index = member.shard().index();
	if let Some(core) = core {
		if moq_sock::cpu::pin(core) {
			tracing::debug!(index, core = core.id(), "pinned io_uring QUIC worker");
		} else {
			tracing::warn!(index, core = core.id(), "failed to pin io_uring QUIC worker");
		}
	}

	let setup = (|| -> anyhow::Result<(moq_uring::Worker, moq_uring::quic::Endpoint)> {
		let mut config = moq_uring::Config::default();
		config.metrics = metrics;
		// Built inside the shared runtime, so the worker keeps that runtime's clock.
		let worker = {
			let _shared = serve.tokio.enter();
			moq_uring::Worker::new(config).context("io_uring setup failed")?
		};
		// The member carries its slot in the steered group, so adopting it is
		// what makes the endpoint issue connection ids that steer back here.
		let socket = worker
			.handle()
			.udp(member, udp)
			.context("failed to adopt the worker socket")?;
		let mut endpoint_config = moq_uring::quic::endpoint::Config::default().with_server(server);
		endpoint_config.admission = serve.shutdown.admission.clone();
		endpoint_config.handshake.timeout = serve.timeout;
		endpoint_config.reset_secret = serve.reset_secret;
		let endpoint =
			moq_uring::quic::Endpoint::new(endpoint_config, socket).context("failed to build the QUIC endpoint")?;
		Ok((worker, endpoint))
	})();

	let (mut worker, endpoint) = match setup {
		Ok(ready_parts) => ready_parts,
		Err(err) => {
			let _ = ready.send(Err(err));
			return false;
		}
	};
	let _ = ready.send(Ok(()));
	if start.recv().is_err() {
		return false;
	}

	let handle = worker.handle();
	let accept_handle = handle.clone();
	let accept_failures = failures.clone();
	handle.spawn(async move {
		loop {
			match endpoint.accept().await {
				Ok(conn) => {
					accept_handle.spawn(serve_connection(accept_handle.clone(), conn, serve.clone()));
				}
				Err(err) => {
					let _ = accept_failures
						.send(anyhow::Error::new(err).context(format!("worker {index} stopped accepting")));
					return;
				}
			}
		}
	});

	if let Err(err) = worker.block_on(stop.wait()) {
		let _ = failures.send(anyhow::Error::new(err).context(format!("worker {index} ring failed")));
	}
	tracing::debug!(index, "io_uring QUIC worker stopped");
	true
}

/// Accept one connection on its worker, then serve it through [`Connection`].
async fn serve_connection(handle: moq_uring::Handle, conn: moq_uring::quic::Connection, serve: Serve) {
	let remote = conn.remote_address();
	// Like the tokio listener, a connection that never reaches its SETUP is no session.
	let request = match moq_tokio::panic::isolate(accept(handle, conn, &serve)).await {
		Some(Ok(request)) => request,
		Some(Err(err)) => return tracing::debug!(%remote, %err, "failed to accept session"),
		// `isolate` logged the panic; only this handshake is lost.
		None => return,
	};
	Connection::new(request, serve.cluster.clone(), serve.auth.clone())
		.with_id(serve.cluster.next_connection_id())
		.with_shutdown(serve.shutdown.clone())
		.with_sessions(serve.sessions.clone())
		.run()
		.await;
}

/// Run the transport handshake and read the moq SETUP, by the deadline the endpoint
/// started at the connection's first packet and enforced through the QUIC handshake.
async fn accept(
	handle: moq_uring::Handle,
	conn: moq_uring::quic::Connection,
	serve: &Serve,
) -> anyhow::Result<Request> {
	let deadline = conn.handshake().deadline;

	// Read the link facts now: the handshake below consumes the connection.
	let remote = conn.remote_address();
	let sni = conn.server_name().map(str::to_owned);
	// TLS validated this against the configured roots before the connection
	// existed, so a chain here is an authenticated peer.
	let identity = conn.peer_identity().map(|chain| serve.tls.peer(chain));

	// Browsers speak WebTransport; everyone else raw QUIC. The moq version
	// rides the ALPN for raw QUIC and the WebTransport subprotocol for `h3`.
	let mut alpn = conn.protocol().map(str::to_owned);
	let (conn, url) = match conn.protocol() {
		Some("h3") => {
			let request = within(deadline, moq_uring::quic::web::Request::accept(conn))
				.await
				.context("WebTransport handshake failed")?;
			let url = request.url.clone();
			let protocol = request
				.protocols
				.iter()
				.find(|offered| serve.alpns.iter().any(|alpn| alpn == *offered))
				.cloned();
			let mut response = web_transport_proto::ConnectResponse::OK;
			if let Some(protocol) = &protocol {
				response = response.with_protocol(protocol);
			}
			alpn = protocol;
			let session = within(deadline, request.respond(response))
				.await
				.context("WebTransport response failed")?;
			(session, Some(url))
		}
		_ => (moq_uring::quic::web::Session::raw(conn), None),
	};

	let session = moq_uring::transport::Local::new(conn.clone());
	let handshake = serve
		.moq
		.accept_request(moq_net::time::Instant::now(), session)
		.with_deadline(deadline)
		.await
		.context("moq handshake failed")?;
	Ok(Request {
		handle,
		shared: serve.tokio.clone(),
		conn,
		handshake,
		local: serve.local,
		remote,
		sni,
		identity,
		alpn,
		url,
		caps: kio::Producer::new(None),
		quic_limits: Default::default(),
	})
}

/// Finish a transport handshake by `deadline`, or fail with a timeout.
///
/// Nothing is established yet that could carry a close code, so an expired
/// handshake is dropped, which closes its connection.
async fn within<T, E: Into<anyhow::Error>>(
	deadline: Option<moq_net::time::Instant>,
	handshake: impl Future<Output = Result<T, E>>,
) -> anyhow::Result<T> {
	let done = match deadline {
		Some(deadline) => kio::clock::timeout_at(deadline, handshake)
			.await
			.map_err(|_| moq_net::Error::Timeout)?,
		None => handshake.await,
	};
	done.map_err(Into::into)
}

/// A session on an io_uring worker whose SETUP awaits the relay's reply.
///
/// It never leaves its worker, which drives the connection and, once the session
/// starts, its protocol; admission and supervision hop to the shared runtime.
pub(crate) struct Request {
	handle: moq_uring::Handle,
	/// The runtime that owns authentication and session supervision.
	shared: tokio::runtime::Handle,
	conn: moq_uring::quic::web::Session,
	handshake: moq_net::server::Handshake,
	local: SocketAddr,
	remote: SocketAddr,
	sni: Option<String>,
	identity: Option<moq_tokio::tls::PeerIdentity>,
	alpn: Option<String>,
	/// The request URL, on WebTransport only.
	url: Option<url::Url>,
	/// The latest caps the lease paced, `None` until it paces any. The lease lives on the
	/// shared runtime, so a task on this worker applies them to the connection.
	caps: kio::Producer<Option<auth::Caps>>,
	/// What [`ok`](crate::incoming::Sealed::ok) applies before answering SETUP.
	quic_limits: moq_quic::Limits,
}

impl crate::incoming::Sealed for Request {
	/// Spawned on the worker by [`ok`](Self::ok).
	type Driver = ();

	fn remote(&self) -> Option<SocketAddr> {
		Some(self.remote)
	}

	fn transport(&self) -> &'static str {
		self.auth_transport().as_str()
	}

	fn shared(&self) -> Option<&tokio::runtime::Handle> {
		Some(&self.shared)
	}

	fn admission(&self, auth: &auth::Auth) -> moq_auth::Request {
		let mut request = auth::request_for_setup(auth, &self.handshake, self.auth_transport());
		// WebTransport addresses its path through the URL; raw QUIC carries it in SETUP.
		if let Some(url) = &self.url {
			request.path = url.path().to_string();
			request.query = url.query().map(str::to_owned);
		}
		request.remote = Some(self.remote);
		request.local = Some(self.local);
		// Only the SNI, like the tokio listener: the CONNECT host selected no certificate.
		request.server_name = self.sni.clone();
		request.host = self
			.url
			.as_ref()
			.and_then(|url| url.host_str())
			.map(str::to_owned)
			.or_else(|| self.sni.clone());
		request.alpn = self.alpn.clone();
		request.tls = self.identity.as_ref().and_then(auth::peer);
		request
	}

	async fn expired(&mut self) {
		self.handshake.expired().await
	}

	async fn reject(self, code: moq_net::SessionError) {
		self.handshake.close(code.into());
	}

	fn pacer(&self) -> Option<auth::Pacer> {
		let caps = self.caps.clone();
		Some(auth::Pacer::new(move |new| {
			if let Ok(mut caps) = caps.write() {
				*caps = Some(new);
			}
		}))
	}

	fn with_quic_limits(mut self, limits: moq_quic::Limits) -> anyhow::Result<Self> {
		limits.validate()?;
		self.quic_limits = limits;
		Ok(self)
	}

	fn auth(&self) -> moq_net::auth::Handle {
		self.handshake.auth()
	}

	fn requests(&mut self) -> moq_net::auth::Requests {
		self.handshake.requests()
	}

	fn role(&self) -> Option<moq_net::setup::Role> {
		self.handshake.role()
	}

	fn peer_hop(&self) -> Option<std::num::NonZeroU64> {
		self.handshake.peer_hop()
	}

	fn with_stats(mut self, stats: moq_net::stats::Session) -> Self {
		self.handshake = self.handshake.with_stats(stats);
		self
	}

	fn with_limits(mut self, limits: moq_net::session::Limits) -> Self {
		self.handshake = self.handshake.with_limits(limits);
		self
	}

	fn with_publisher(mut self, publish: moq_net::origin::Consumer) -> Self {
		self.handshake = self.handshake.with_publisher(publish);
		self
	}

	fn with_subscriber(mut self, subscribe: moq_net::origin::Producer) -> Self {
		self.handshake = self.handshake.with_subscriber(subscribe);
		self
	}

	async fn ok(self) -> anyhow::Result<(moq_net::Session, Self::Driver)> {
		let Self {
			handle,
			conn,
			mut handshake,
			caps,
			mut quic_limits,
			..
		} = self;
		// The other limits go first; moq-net opens the receive window after them, so a
		// `receive_rate` paces the opened window.
		if let Some(window) = quic_limits.receive_window.take() {
			handshake = handshake.with_receive_window(window);
		}
		conn.set_limits(quic_limits).expect("validated by with_quic_limits");

		// Holds only a consumer: dropping the lease, and with it the pacer, closes `caps`,
		// which ends the task and releases its handle on the connection.
		let caps = caps.consume();
		let pacing = {
			let conn = conn.clone();
			async move {
				let mut applied = None;
				while let Ok(next) = caps
					.wait(|caps| match **caps != applied {
						true => std::task::Poll::Ready(**caps),
						false => std::task::Poll::Pending,
					})
					.await
				{
					if let Some(caps) = next
						&& let Err(err) = conn.set_limits(caps.limits())
					{
						tracing::warn!(%err, "failed to apply bitrate caps");
					}
					applied = next;
				}
			}
		};
		handle.spawn(pacing.in_current_span());

		let (session, driver) = handshake.ok().await?;
		let driver_handle = handle.clone();
		let driver = async move {
			match driver_handle.run(driver).await {
				moq_net::Error::Closed => {}
				err => tracing::debug!(%err, "session driver ended"),
			}
		};
		handle.spawn(moq_tokio::panic::isolate(driver).in_current_span().map(drop));
		Ok((session, ()))
	}

	async fn drive((): Self::Driver) -> anyhow::Result<()> {
		Ok(())
	}
}

impl Request {
	/// WebTransport when the request carried a URL, else raw QUIC.
	fn auth_transport(&self) -> moq_auth::Transport {
		match self.url {
			Some(_) => moq_auth::Transport::WebTransport,
			None => moq_auth::Transport::Quic,
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Unsupported listen settings fail before loading TLS or binding a socket.
	#[test]
	fn bind_refuses_preferred_addresses() {
		moq_tokio::crypto::install_default().unwrap();
		type Set = fn(&mut moq_tokio::listen::Config);
		let cases: [(&str, Set); 2] = [
			("listen.preferred_v4", |listen| {
				listen.preferred_v4 = Some("192.0.2.1:443".parse().unwrap());
			}),
			("listen.preferred_v6", |listen| {
				listen.preferred_v6 = Some("[2001:db8::1]:443".parse().unwrap());
			}),
		];
		for (setting, set) in cases {
			for configured in [false, true] {
				let mut server = moq_tokio::server::Config::default();
				if configured {
					server.listen.bind = Some("127.0.0.1:0".parse().unwrap());
					server.listen.tls.generate = vec!["localhost".into()];
				}
				set(&mut server.listen);
				let err = Workers::bind(server, moq_tokio::worker::Config::new(1))
					.expect_err("the workers cannot advertise a preferred address");
				assert!(err.to_string().contains(setting), "{err:#}");
			}
		}
	}

	/// The workers refuse what the tokio listener refuses, before binding a socket.
	#[test]
	fn bind_refuses_an_invalid_listen() {
		moq_tokio::crypto::install_default().unwrap();
		let mut server = moq_tokio::server::Config::default();
		server.listen.bind = Some("127.0.0.1:0".parse().unwrap());
		server.listen.tls.generate = vec!["localhost".into()];
		server.listen.lb.nonce = Some(8);
		let err = Workers::bind(server, moq_tokio::worker::Config::new(1))
			.expect_err("a QUIC-LB nonce without an id is refused");
		assert!(err.to_string().contains("--listen-quic-lb-nonce"), "{err:#}");
	}

	/// A window the operator set has to reach the worker. Refusing it, or
	/// accepting the config and leaving the field unset, would both hide it.
	///
	/// `Config` is non-exhaustive, so the windows are filled in after `default`
	/// rather than in a struct literal.
	#[test]
	fn windows_are_applied() {
		let mut quic = moq_tokio::quic::Config::default();
		quic.receive_window = Some(128 << 20);
		quic.stream_receive_window = Some(8 << 20);
		quic.send_window = Some(32 << 20);

		let transport = transport(&quic.resolve()).expect("windows must be accepted");
		assert_eq!(transport.limits.receive_window, NonZeroU64::new(128 << 20));
		assert_eq!(transport.limits.stream_receive_window, NonZeroU64::new(8 << 20));
		assert_eq!(transport.limits.send_window, NonZeroU64::new(32 << 20));
	}

	/// `quic.mtu_discovery` reaches the workers as it does the tokio backends: off unless set.
	#[test]
	fn mtu_discovery_is_applied() {
		let mut quic = moq_tokio::quic::Config::default();
		assert!(!transport(&quic.resolve()).expect("default").mtu_discovery);
		quic.mtu_discovery = Some(true);
		assert!(transport(&quic.resolve()).expect("discovery").mtu_discovery);
	}

	/// Incomplete messages hold connection credit, so the window has to fit the largest.
	#[test]
	fn a_receive_window_below_the_largest_message_is_refused() {
		let min = moq_net::transport::MIN_RECEIVE_WINDOW;
		let mut quic = moq_tokio::quic::Config::default();
		quic.receive_window = Some(min - 1);
		let err = transport(&quic.resolve()).expect_err("below the largest message");
		assert!(err.to_string().contains("largest message"), "{err}");

		quic.receive_window = Some(min);
		assert_eq!(
			transport(&quic.resolve()).expect("the minimum").limits.receive_window,
			NonZeroU64::new(min)
		);
	}

	/// The same `[quic]` section runs the same windows on both runtimes, set or
	/// left at the documented defaults: the tokio backends apply `Resolved`'s
	/// windows as they are, and the workers must get exactly those.
	#[test]
	fn windows_match_the_tokio_runtime() {
		let mut set = moq_tokio::quic::Config::default();
		set.receive_window = Some(256 << 20);
		set.stream_receive_window = Some(8 << 20);
		set.send_window = Some(32 << 20);

		for (quic, expected) in [
			(moq_tokio::quic::Config::default(), (4 << 20, 1 << 20, 4 << 20)),
			(set, (256 << 20, 8 << 20, 32 << 20)),
		] {
			let resolved = quic.resolve();
			let tokio = (
				resolved.receive_window,
				resolved.stream_receive_window,
				resolved.send_window,
			);
			assert_eq!(tokio, expected);

			let transport = transport(&resolved).expect("windows must be accepted");
			let uring = (
				transport.limits.receive_window,
				transport.limits.stream_receive_window,
				transport.limits.send_window,
			);
			assert_eq!(
				uring,
				(
					NonZeroU64::new(tokio.0),
					NonZeroU64::new(tokio.1),
					NonZeroU64::new(tokio.2)
				)
			);
		}
	}

	/// A direct `bind` validates `[quic]` too, so a key log nobody can read back fails
	/// instead of binding and logging nothing.
	#[test]
	fn bind_refuses_an_empty_keylog_path() {
		let mut server = moq_tokio::server::Config::default();
		server.quic.keylog = Some(std::path::PathBuf::new());
		let err =
			Workers::bind(server, moq_tokio::worker::Config::new(1)).expect_err("an empty keylog path must be refused");
		assert!(
			matches!(err.downcast_ref(), Some(moq_tokio::Error::KeylogUnset)),
			"{err:#}"
		);
	}

	/// A group stopped before it serves, as when a later startup step fails, ends
	/// the workers waiting to serve instead of hanging on their join.
	#[tokio::test]
	async fn a_group_stopped_before_serving_joins() {
		moq_tokio::crypto::install_default().unwrap();
		if let Err(moq_uring::Error::Unsupported(reason)) = moq_uring::Worker::new(Default::default())
			&& std::env::var_os("CI_STRICT").is_none()
		{
			eprintln!("skipping io_uring relay test: {reason}");
			return;
		}

		let mut server = moq_tokio::server::Config::default();
		server.listen.bind = Some("127.0.0.1:0".parse().unwrap());
		server.listen.tls.generate = vec!["localhost".into()];
		let workers = Workers::bind(server, moq_tokio::worker::Config::new(2)).unwrap();
		let cluster = cluster::Cluster::spawn(cluster::Options::default()).unwrap();
		let workers = workers
			.spawn(
				cluster,
				auth::Auth::refuse("test"),
				shutdown::Observer::disabled(),
				Default::default(),
			)
			.unwrap();
		workers.shutdown().await;
	}

	/// An auth decision still pending at the drain deadline is abandoned by then.
	#[tokio::test]
	async fn pending_admission_is_abandoned_at_the_deadline() {
		moq_tokio::crypto::install_default().unwrap();
		if let Err(moq_uring::Error::Unsupported(reason)) = moq_uring::Worker::new(Default::default())
			&& std::env::var_os("CI_STRICT").is_none()
		{
			eprintln!("skipping io_uring relay test: {reason}");
			return;
		}

		let dir = tempfile::tempdir().unwrap();
		let key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
		let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
			.map(|mut params| {
				params.serial_number = Some(1u64.into());
				params
			})
			.unwrap()
			.self_signed(&key)
			.unwrap();
		let (cert_path, key_path) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
		std::fs::write(&cert_path, cert.pem()).unwrap();
		std::fs::write(
			&key_path,
			pem::encode(&pem::Pem::new("PRIVATE KEY", key.to_pkcs8().secret_pkcs8_der())),
		)
		.unwrap();

		let window = std::time::Duration::from_secs(1);
		let mut config = crate::Config::default();
		config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
		config.listen.tls.cert = vec![cert_path];
		config.listen.tls.key = vec![key_path];
		config.runtime.workers = crate::runtime::Workers::Count(std::num::NonZeroU16::new(1).unwrap());
		config.runtime.pin = false;
		config.runtime.io_uring = true;
		config.drain_timeout = Some(window.into());
		let mut relay = crate::Relay::load(config).await.unwrap().with_signals(false);
		let port = relay.addr().unwrap().port();
		let mut admissions = relay.admissions().unwrap();
		let trigger = relay.shutdown_trigger().clone();
		let ready = relay.ready();
		let running = tokio::spawn(relay.run());
		ready.wait().await.expect("relay ready");

		let mut client = moq_tokio::connect::Config::default();
		client.tls.insecure = Some(true);

		client.bind = Some("127.0.0.1:0".parse().unwrap());
		let url: url::Url = format!("moql://127.0.0.1:{port}/drain").parse().unwrap();
		let client = client.init(Default::default()).unwrap();
		let connecting = tokio::spawn(async move { client.with_reconnect(false).connect(url).established().await });
		let mut admission = admissions.next().await.unwrap();

		// Only timers decide what follows; the embedded admission's own timeout
		// is well past the drain window.
		tokio::time::pause();
		trigger.start();
		kio::clock::timeout(window + std::time::Duration::from_secs(1), admission.abandoned())
			.await
			.expect("the admission outlived the drain deadline");
		running.await.unwrap().unwrap();
		connecting.abort();
	}

	/// `listen.timeout` bounds admission on the io_uring listener too: a decision
	/// that never comes is abandoned at the handshake deadline.
	#[tokio::test]
	async fn pending_admission_is_abandoned_at_the_handshake_deadline() {
		moq_tokio::crypto::install_default().unwrap();
		if let Err(moq_uring::Error::Unsupported(reason)) = moq_uring::Worker::new(Default::default())
			&& std::env::var_os("CI_STRICT").is_none()
		{
			eprintln!("skipping io_uring relay test: {reason}");
			return;
		}

		let dir = tempfile::tempdir().unwrap();
		let key = rind_rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
		let cert = rcgen::CertificateParams::new(vec!["localhost".to_string()])
			.map(|mut params| {
				params.serial_number = Some(1u64.into());
				params
			})
			.unwrap()
			.self_signed(&key)
			.unwrap();
		let (cert_path, key_path) = (dir.path().join("cert.pem"), dir.path().join("key.pem"));
		std::fs::write(&cert_path, cert.pem()).unwrap();
		std::fs::write(
			&key_path,
			pem::encode(&pem::Pem::new("PRIVATE KEY", key.to_pkcs8().secret_pkcs8_der())),
		)
		.unwrap();

		let timeout = std::time::Duration::from_secs(1);
		let mut config = crate::Config::default();
		config.listen.bind = Some("127.0.0.1:0".parse().unwrap());
		config.listen.tls.cert = vec![cert_path];
		config.listen.tls.key = vec![key_path];
		config.listen.timeout = Some(timeout.into());
		config.runtime.workers = crate::runtime::Workers::Count(std::num::NonZeroU16::new(1).unwrap());
		config.runtime.pin = false;
		config.runtime.io_uring = true;
		config.drain_timeout = Some(std::time::Duration::from_millis(100).into());
		let mut relay = crate::Relay::load(config).await.unwrap().with_signals(false);
		let port = relay.addr().unwrap().port();
		let mut admissions = relay.admissions().unwrap();
		let trigger = relay.shutdown_trigger().clone();
		let ready = relay.ready();
		let running = tokio::spawn(relay.run());
		ready.wait().await.expect("relay ready");

		let mut client = moq_tokio::connect::Config::default();
		client.tls.insecure = Some(true);

		client.bind = Some("127.0.0.1:0".parse().unwrap());
		let url: url::Url = format!("moql://127.0.0.1:{port}/deadline").parse().unwrap();
		let client = client.init(Default::default()).unwrap();
		let connecting = tokio::spawn(async move { client.with_reconnect(false).connect(url).established().await });
		let mut admission = admissions.next().await.unwrap();

		// The handshake deadline is an io_uring timer, which a paused tokio clock cannot
		// advance, so this waits out the real second with nobody answering.
		kio::clock::timeout(timeout * 5, admission.abandoned())
			.await
			.expect("the admission outlived the handshake deadline");
		trigger.start();
		running.await.unwrap().unwrap();
		connecting.abort();
	}
}
