//! Serving QUIC from io_uring workers.
//!
//! The [`moq_uring`] counterpart of [`moq_tokio::worker`]: one pinned thread
//! per worker, each with its own ring, its own `SO_REUSEPORT` socket steered
//! by connection id ([`moq_sock::shard`]), and its own
//! [`moq_uring::quic::Endpoint`] serving browsers (WebTransport over `h3`)
//! and native peers (raw QUIC, moq-lite ALPNs) alike. Sessions are moq-lite
//! only.
//!
//! The split of work mirrors the relay's topology: a worker owns everything
//! transport-shaped (the handshake, the session's protocol machine), while
//! authentication and session supervision run on the shared tokio runtime,
//! which owns the HTTP client, the timers, and the origins. The session
//! handle is `Send + Sync` whatever drives its transport, which is what makes
//! the handoff free.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use anyhow::Context as _;

use crate::{Auth, AuthParams, Cluster, Shutdown};

/// How many workers the steering filter can address.
const MAX_WORKERS: u16 = moq_sock::shard::MAX_SHARDS;

/// One member's bound socket and its slot in the steered group.
struct Member {
	shard: moq_sock::shard::Shard,
	socket: std::net::UdpSocket,
}

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
			*self.waker.lock().unwrap() = Some(cx.waker().clone());
			if self.stopped.load(Ordering::Acquire) {
				return Poll::Ready(());
			}
			Poll::Pending
		})
		.await
	}

	fn stop(&self) {
		self.stopped.store(true, Ordering::Release);
		if let Some(waker) = self.waker.lock().unwrap().take() {
			waker.wake();
		}
	}
}

/// Everything a worker needs to serve a connection, cloned per thread.
#[derive(Clone)]
struct Serve {
	cluster: Cluster,
	auth: Auth,
	shutdown: Shutdown,
	/// The shared runtime, which owns authentication (the auth API's HTTP
	/// client needs its reactor) and session supervision.
	tokio: tokio::runtime::Handle,
	/// The moq-lite ALPNs this listener speaks, for the WebTransport
	/// subprotocol pick.
	alpns: Arc<Vec<String>>,
	/// The negotiated-version restriction, when the operator set one.
	versions: moq_net::Versions,
}

/// A bound group of io_uring QUIC workers sharing one port.
///
/// [`bind`](Self::bind) opens every socket, so a returned value already owns
/// the listen address and a port conflict is an error here rather than a
/// worker that quietly died. Nothing is served until [`serve`](Self::serve)
/// spawns the threads.
pub struct Workers {
	members: Vec<Member>,
	addr: SocketAddr,
	server: moq_uring::quic::server::Config,
	udp: moq_uring::udp::Config,
	pin: bool,
	alpns: Arc<Vec<String>>,
	versions: moq_net::Versions,

	threads: Vec<(u16, std::thread::JoinHandle<()>)>,
	stops: Vec<Arc<Stop>>,
	/// A worker's fatal error, reported to whoever polls [`failed`](Self::failed).
	failures: tokio::sync::mpsc::UnboundedReceiver<anyhow::Error>,
	failures_tx: tokio::sync::mpsc::UnboundedSender<anyhow::Error>,

	/// Holds the listen port against a second group for as long as this one
	/// lives.
	_lock: Option<moq_sock::shard::Lock>,
}

impl Workers {
	/// Bind one socket per worker on the address `listen` describes.
	///
	/// Like the tokio group, `listen`'s `tcp`/`unix` listeners are ignored
	/// (serve those from an [`init_streams`](moq_tokio::listen::Config::init_streams)
	/// server), and `tls.generate` is refused since every worker must serve
	/// one identity. Unlike it, exactly one certificate/key pair is required,
	/// and mTLS client roots are not yet wired through.
	pub fn bind(
		listen: &moq_tokio::listen::Config,
		quic: &moq_tokio::quic::Config,
		config: moq_tokio::worker::Config,
	) -> anyhow::Result<Self> {
		if !listen.tls.generate.is_empty() {
			anyhow::bail!("io_uring workers cannot use listen.tls.generate; provide a certificate file");
		}

		// Everything below is a setting this listener cannot deliver. Refusing
		// beats starting and quietly behaving differently from what the
		// operator configured, which is the whole failure mode this mode is
		// most likely to produce.
		if !listen.tls.root.is_empty() {
			anyhow::bail!(
				"io_uring workers do not implement mTLS client roots (listen.tls.root); \
				 use token auth, or the tokio workers"
			);
		}
		if listen.lb_id.is_some() {
			anyhow::bail!(
				"io_uring workers issue shard-steered connection ids and cannot also carry a \
				 QUIC-LB server id (listen.lb_id)"
			);
		}
		if let Some(backend) = listen.backend.as_ref() {
			anyhow::bail!("io_uring workers serve their own QUIC stack; listen.backend={backend:?} cannot apply");
		}
		let (cert, key) = match (listen.tls.cert.as_slice(), listen.tls.key.as_slice()) {
			([cert], [key]) => (cert.clone(), key.clone()),
			([], []) => anyhow::bail!("io_uring workers need a certificate (listen.tls.cert/key)"),
			_ => anyhow::bail!("io_uring workers serve exactly one certificate, got several"),
		};

		let count = config.count.max(1);
		if count > MAX_WORKERS {
			anyhow::bail!("at most {MAX_WORKERS} io_uring workers, got {count}");
		}

		// One resolution for the whole group, so a DNS answer that rotates
		// between queries cannot hand members different addresses.
		//
		// Named rather than defaulted: an unset `bind` means stream-only when
		// a tcp/unix listener is configured, and defaulting to the usual
		// address here would open a QUIC listener nobody asked for.
		let requested: SocketAddr = {
			use std::net::ToSocketAddrs;
			let bind = listen
				.bind
				.as_deref()
				.context("io_uring workers need an explicit listen.bind")?;
			bind.to_socket_addrs()
				.with_context(|| format!("failed to resolve {bind}"))?
				.next()
				.with_context(|| format!("{bind} resolved to no address"))?
		};

		// Excludes a concurrently-constructing same-UID group; the bind probe
		// inside `shard::bind` excludes one that already holds the port.
		let lock = match requested.port() {
			0 => None,
			port => moq_sock::shard::Lock::acquire(port)
				.map_err(|_| anyhow::anyhow!("another QUIC worker group already owns {requested}"))?,
		};

		let mut members = Vec::with_capacity(count as usize);
		let mut addr = requested;
		for index in 0..count {
			let shard = moq_sock::shard::Shard::new(index, count).expect("index is below count");
			let socket = moq_sock::shard::bind(addr, Some(shard))
				.with_context(|| format!("failed to bind worker {index} on {addr}"))?;
			let bound = socket.local_addr().context("bound socket has no address")?;
			// An ephemeral request gives each worker a port of its own; the
			// group only balances a port every member actually holds.
			if index == 0 {
				addr = bound;
			} else if bound != addr {
				anyhow::bail!("worker {index} bound {bound}, the group holds {addr}; use a fixed port");
			}
			members.push(Member { shard, socket });
		}

		// The moq-lite ALPNs this listener speaks: the operator's version
		// restriction when one is set, minus anything that is not lite (the
		// io_uring path runs unboxed lite machines only). `h3` rides along
		// for browsers, which negotiate the moq version as a WebTransport
		// subprotocol instead.
		let versions: moq_net::Versions = match listen.version.is_empty() {
			true => Default::default(),
			false => listen.version.clone().into(),
		};
		let alpns: Vec<String> = versions
			.iter()
			.filter(|version| version.is_lite())
			.map(|version| version.alpn().to_string())
			.collect();
		if alpns.is_empty() {
			anyhow::bail!("io_uring workers speak moq-lite only, and the version restriction leaves none");
		}
		// lite 01/02 have no ALPN of their own: they negotiate over the shared
		// `moql` one through the bidi SETUP exchange, which
		// `accept_request_lite` does not implement. The default set carries
		// them and simply does not advertise them, like the moq-transport
		// versions below; asking for one *by name* is the case that would
		// start cleanly and then fail every handshake.
		if !listen.version.is_empty()
			&& let Some(version) = versions.iter().find(|version| version.uses_setup_negotiation())
		{
			anyhow::bail!("io_uring workers cannot serve {version:?}, which negotiates its version in the SETUP");
		}
		if versions.iter().any(|version| !version.is_lite()) {
			tracing::warn!(
				"io_uring workers speak moq-lite only; the configured moq-transport versions are not served"
			);
		}

		let mut server = moq_uring::quic::server::Config::new(moq_uring::quic::Identity::new(cert, key));
		server.alpn = alpns.iter().cloned().chain(["h3".to_string()]).collect();
		let quic = quic.resolve();
		server.transport = transport(&quic)?;

		// GSO is a property of the socket, not the connection, so it rides the
		// worker's UDP config rather than quiche's.
		let mut udp = moq_uring::udp::Config::default();
		udp.gso = quic.gso.unwrap_or(true);

		let (failures_tx, failures) = tokio::sync::mpsc::unbounded_channel();
		tracing::info!(workers = count, %addr, "bound io_uring QUIC workers");

		Ok(Self {
			members,
			addr,
			server,
			udp,
			pin: config.pin,
			alpns: Arc::new(alpns),
			versions,
			threads: Vec::new(),
			stops: Vec::new(),
			failures,
			failures_tx,
			_lock: lock,
		})
	}

	/// The address every worker is bound to.
	pub fn local_addr(&self) -> SocketAddr {
		self.addr
	}

	/// Spawn the worker threads and start accepting.
	///
	/// Call from the shared runtime: it is captured as the home for
	/// authentication and session supervision. Returns once every worker is
	/// serving; a worker that cannot start (an old kernel, a ring failure) is
	/// an error here rather than a thread that quietly died.
	pub fn serve(&mut self, cluster: Cluster, auth: Auth, shutdown: Shutdown) -> anyhow::Result<()> {
		let serve = Serve {
			cluster,
			auth,
			shutdown,
			tokio: tokio::runtime::Handle::current(),
			alpns: self.alpns.clone(),
			versions: self.versions.clone(),
		};

		let cores = match self.pin {
			true => moq_sock::cpu::cores(),
			false => Vec::new(),
		};

		for member in std::mem::take(&mut self.members) {
			let index = member.shard.index();
			let core = cores.get(index as usize % cores.len().max(1)).copied();
			let stop = Arc::new(Stop::default());
			let (ready_tx, ready_rx) = std::sync::mpsc::channel();

			let spawn = Spawn {
				member,
				core,
				server: self.server.clone(),
				udp: self.udp.clone(),
				serve: serve.clone(),
				stop: stop.clone(),
				ready: ready_tx,
				failures: self.failures_tx.clone(),
			};
			let thread = std::thread::Builder::new()
				.name(format!("moq-uring-{index}"))
				.spawn(move || run_worker(spawn))
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
		}

		Ok(())
	}

	/// The first worker's fatal error. Pends until one dies, which a healthy
	/// group never does, so this composes into a `select!` loop.
	pub async fn failed(&mut self) -> anyhow::Error {
		match self.failures.recv().await {
			Some(err) => err,
			// Senders live in `self`, so this is unreachable; pend regardless.
			None => std::future::pending().await,
		}
	}

	/// Stop every worker and wait for its thread, off the caller's runtime.
	pub async fn shutdown(mut self) {
		let threads = self.signal();
		if threads.is_empty() {
			return;
		}
		let _ = tokio::task::spawn_blocking(move || join(threads)).await;
	}

	fn signal(&mut self) -> Vec<(u16, std::thread::JoinHandle<()>)> {
		for stop in &self.stops {
			stop.stop();
		}
		std::mem::take(&mut self.threads)
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
fn transport(quic: &moq_tokio::quic::Resolved) -> anyhow::Result<moq_uring::quic::Transport> {
	anyhow::ensure!(
		quic.qlog.is_none(),
		"io_uring workers cannot write qlog traces; drop quic.qlog or use the tokio workers"
	);
	// The datagram path fixes both payload ceilings at SEGMENT and hands
	// `conn.send` slices of exactly that, so discovery has no larger size to
	// find and would only add probes.
	anyhow::ensure!(
		!quic.mtu_discovery,
		"io_uring workers send a fixed-size UDP payload, so quic.mtu_discovery has nothing to discover"
	);

	let mut transport = moq_uring::quic::Transport::default();
	transport.idle_timeout = quic.idle_timeout;
	transport.max_streams = quic.max_streams;
	transport.keep_alive = quic.keep_alive;
	transport.congestion = match quic.congestion_control {
		Some(moq_tokio::quic::CongestionControl::Loss) => moq_uring::quic::Congestion::Loss,
		// Unset means the backend's own default, and live media wants a steady
		// send rate an encoder can track over CUBIC's sawtooth.
		Some(moq_tokio::quic::CongestionControl::Delay) | None => moq_uring::quic::Congestion::Delay,
		// A family added to the tokio stack later, rather than a silent
		// substitution the operator would never learn about.
		Some(other) => anyhow::bail!("io_uring workers cannot run the {other:?} congestion controller"),
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
	member: Member,
	/// The core to pin to, or `None` when pinning is off or unavailable.
	core: Option<moq_sock::cpu::CoreId>,
	server: moq_uring::quic::server::Config,
	udp: moq_uring::udp::Config,
	serve: Serve,
	stop: Arc<Stop>,
	/// Reports setup back to [`Workers::serve`], which blocks on it so a
	/// worker that cannot start is an error there rather than a dead thread.
	ready: std::sync::mpsc::Sender<anyhow::Result<()>>,
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
	let index = spawn.member.shard.index();
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
		// `Workers::serve` is blocking on.
		Ok(_) => {}
	}
}

/// Pin, ring up, and serve the endpoint until stopped. Returns whether the
/// worker ever got as far as serving.
fn serve_worker(spawn: Spawn) -> bool {
	let Spawn {
		member,
		core,
		server,
		udp,
		serve,
		stop,
		ready,
		failures,
	} = spawn;
	let index = member.shard.index();
	if let Some(core) = core {
		if moq_sock::cpu::pin(core) {
			tracing::debug!(index, core = core.id(), "pinned io_uring QUIC worker");
		} else {
			tracing::warn!(index, core = core.id(), "failed to pin io_uring QUIC worker");
		}
	}

	let setup = (|| -> anyhow::Result<(moq_uring::Worker, moq_uring::quic::Endpoint)> {
		let worker = moq_uring::Worker::new(Default::default()).context("io_uring setup failed")?;
		let handle = worker.handle();
		let socket = handle
			.udp(member.socket, udp)
			.context("failed to adopt the worker socket")?;
		let endpoint = moq_uring::quic::Endpoint::new(
			&handle,
			socket,
			moq_uring::quic::endpoint::Config::default()
				.with_server(server)
				.with_shard(member.shard),
		)
		.context("failed to build the QUIC endpoint")?;
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

	let handle = worker.handle();
	let accept_handle = handle.clone();
	let accept_failures = failures.clone();
	handle.spawn(async move {
		loop {
			match endpoint.accept().await {
				Ok(conn) => {
					let id = serve.cluster.next_connection_id();
					let conn_handle = accept_handle.clone();
					let serve = serve.clone();
					accept_handle.spawn(async move {
						if let Err(err) = serve_connection(conn_handle, conn, id, serve).await {
							tracing::warn!(id, %err, "connection closed");
						}
					});
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

/// Serve one accepted connection on its worker: handshake and session start
/// here; authentication and supervision on the shared runtime.
async fn serve_connection(
	handle: moq_uring::Handle,
	conn: moq_uring::quic::Connection,
	id: u64,
	serve: Serve,
) -> anyhow::Result<()> {
	use web_transport_trait::poll::Session as _;

	// Browsers speak WebTransport; everyone else raw QUIC. The moq version
	// rides the ALPN for raw QUIC and the WebTransport subprotocol for `h3`.
	// A browser offering no subprotocol we speak is refused with
	// `Error::Version` rather than falling back to the pre-lite-05 bidi SETUP
	// the tokio listener still accepts: that path is moq-transport-shaped, and
	// reaching it would drag the thread-affinity bounds `accept_request_lite`
	// exists to avoid back onto this transport.
	let (transport, url) = match conn.protocol() {
		Some("h3") => {
			let request = moq_uring::quic::web::Request::accept(&handle, conn)
				.await
				.context("WebTransport handshake failed")?;
			let url = request.url().clone();
			let protocol = request
				.protocols()
				.iter()
				.find(|offered| serve.alpns.iter().any(|alpn| alpn == *offered))
				.cloned();
			let mut response = moq_uring::quic::web::Response::default();
			if let Some(protocol) = &protocol {
				response = response.with_protocol(protocol);
			}
			let session = request
				.respond(response)
				.await
				.context("WebTransport response failed")?;
			(session, Some(url))
		}
		_ => (moq_uring::quic::web::Session::raw(conn), None),
	};

	let request = moq_net::Server::new()
		.with_versions(serve.versions.clone())
		.accept_request_lite(handle.clone(), transport)
		.await
		.context("moq handshake failed")?;

	// The path + `?jwt=` ride the URL for WebTransport and the SETUP for raw
	// QUIC; either way the verdict comes from the shared runtime, which owns
	// the auth API's HTTP client.
	let mut params = match &url {
		Some(url) => serve.auth.params_from_url(url),
		None => {
			let setup = request.path();
			let (path, query) = match setup.split_once('?') {
				Some((path, query)) => (path, Some(query)),
				None => (setup, None),
			};
			AuthParams::from_path_query(path, query)
		}
	};
	params.transport = Some(moq_tokio::Transport::Quic);

	let auth = serve.auth.clone();
	let token = match serve
		.tokio
		.spawn(async move { auth.verify(&params).await })
		.await
		.context("auth task failed")?
	{
		Ok(token) => token,
		Err(err) => {
			// The status is what separates "your credential is bad" from "the
			// auth API is down". Collapsing both into Unauthorized tells a
			// client to stop reconnecting through an outage it could have
			// waited out.
			let status = axum::http::StatusCode::from(&err);
			request.close(match status {
				axum::http::StatusCode::UNAUTHORIZED | axum::http::StatusCode::FORBIDDEN => {
					moq_net::Error::Unauthorized
				}
				other => moq_net::Error::App(other.as_u16()),
			});
			return Err(anyhow::Error::new(err).context("authentication failed"));
		}
	};

	let role = request.role();
	let grants = match crate::connection::authorize(&serve.cluster, &token, role, &moq_tokio::Transport::Quic) {
		Ok(grants) => grants,
		Err(err) => {
			request.close(moq_net::Error::Unauthorized);
			return Err(err);
		}
	};

	let peer_origin = request.peer_origin();
	let mut request = request.with_stats(grants.stats);
	if let Some(subscribe) = grants.subscribe {
		request = request.with_publisher(&subscribe);
	}
	if let Some(publish) = grants.publish {
		request = request.with_subscriber(publish);
	}
	let session = request.ok().await?;
	let node_connection = peer_origin.map(|origin| serve.cluster.nodes.connect_inbound(id, origin));

	tracing::info!(id, version = %session.version(), transport = %moq_tokio::Transport::Quic, "negotiated");

	// The session handle is Send + Sync however its transport is driven, so
	// its lifecycle (credential expiry, GOAWAY drain) lives with the timers
	// and the shutdown broadcast on the shared runtime.
	let shutdown = serve.shutdown.clone();
	let auth = serve.auth.clone();
	serve.tokio.spawn(async move {
		let _node_connection = node_connection;
		if let Err(err) = crate::connection::supervise(&auth, session, token, shutdown).await {
			tracing::warn!(id, %err, "connection closed");
		}
	});

	Ok(())
}
