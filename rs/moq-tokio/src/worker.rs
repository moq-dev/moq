//! Thread-per-core QUIC workers.
//!
//! A [`Server`] on a work-stealing runtime serves every connection off one UDP
//! socket: every packet can cross threads, and every wakeup is a candidate
//! context switch. [`Workers`] is the opposite shape. Each
//! member is a thread of its own, pinned to a core, running a `current_thread`
//! runtime and owning one socket in a `SO_REUSEPORT` group. A connection lands
//! on one worker and stays there, so the locks its driver and its session take
//! are uncontended and nothing is stolen.
//!
//! Packets reach their worker by connection ID rather than by address, so a
//! client that migrates (a NAT rebinding, a network change) stays with the
//! worker that owns its connection rather than landing on one that has never
//! heard of it.
//!
//! ```no_run
//! # async fn example(listen: moq_tokio::listen::Config) -> anyhow::Result<()> {
//! use moq_tokio::worker;
//!
//! let mut workers = worker::Workers::bind(listen, Default::default(), worker::Config::new(8))?;
//! println!("listening on {}", workers.local_addr());
//!
//! // The group owns the threads, so keep it alive as long as you want the
//! // port served.
//! for (server, spawner) in workers.split() {
//!     spawner.run(async move {
//!         let _ = server.listen().await;
//!     });
//! }
//! workers.shutdown().await;
//! # Ok(())
//! # }
//! ```

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use crate::{Error, Result, Server, listen::Shard};

/// How many QUIC workers to run, and whether to pin them.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct Config {
	/// How many workers to run, each with a thread and a socket of its own.
	pub count: u16,

	/// Pin each worker to a CPU core.
	///
	/// The mode's measured win comes from each worker owning a socket and a
	/// runtime, not from pinning: on a single-socket machine, pinned and unpinned
	/// benchmark inside run-to-run noise of each other. Pinning stops the
	/// scheduler migrating a busy worker, which should matter on a multi-socket
	/// or NUMA machine, and costs nothing elsewhere, so it defaults on. Turn it
	/// off when sharing the machine with something that manages CPU placement
	/// itself.
	pub pin: bool,
}

impl Config {
	/// `count` workers, pinned.
	pub fn new(count: u16) -> Self {
		Self { count, pin: true }
	}

	/// Whether to pin each worker to a core.
	pub fn with_pin(mut self, pin: bool) -> Self {
		self.pin = pin;
		self
	}
}

/// A bound group of QUIC workers sharing one port.
///
/// [`Workers::bind`] opens every socket, so a returned value is already
/// listening and a port conflict is an error here rather than a worker that
/// quietly died. Nothing is accepted until [`Workers::split`] hands each
/// worker's server to the caller and its spawner runs an accept loop.
///
/// Iterate to take the workers out. The group is what guarantees they were bound
/// once, in index order, which is what the steering filter selects on.
pub struct Workers {
	workers: Vec<Worker>,
	certificates: crate::tls::Certificates,
	addr: SocketAddr,
}

impl std::fmt::Debug for Workers {
	/// Hand-written because the bound []s are opaque; the member count and
	/// the shared address is what identifies a group anyway.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Workers")
			.field("workers", &self.workers.len())
			.field("addr", &self.addr)
			.finish()
	}
}

impl Workers {
	/// Bind one socket per worker, on the address `listen` describes.
	///
	/// Returns once every worker is listening. `listen`'s `tcp`/`unix` listeners
	/// are ignored: a group shares one UDP port, and a stream listener cannot be
	/// bound more than once. Serve those from a
	/// [`init_streams`](crate::listen::Config::init_streams) server on the
	/// caller's own runtime.
	pub fn bind(listen: crate::listen::Config, quic: crate::quic::Config, config: Config) -> Result<Self> {
		// Each worker loads the certificate files itself, so generating would give
		// every member a certificate of its own and clients a different one per
		// connection.
		if !listen.tls.generate.is_empty() {
			return Err(Error::WorkerTlsGenerate);
		}

		let count = config.count.max(1);
		if count > crate::steer::MAX_SHARDS {
			return Err(Error::WorkerCount {
				count,
				max: crate::steer::MAX_SHARDS,
			});
		}

		let cores = config.pin.then(cores).unwrap_or_default();

		// One resolution for the whole group. Each worker resolves its own config
		// otherwise, so a DNS answer that rotates between queries would hand
		// members different addresses and fail the bind on whichever member drew a
		// fresh one. An unset `bind` stays unset: the backends fall back to a
		// literal default, and `Some` here would flip a stream-only config into
		// opening a QUIC listener.
		let mut listen = listen;
		if let Some(bind) = listen.bind.as_deref() {
			let addr = crate::util::resolve(Some(bind), bind).map_err(|err| Error::WorkerResolve(Arc::new(err)))?;
			listen.bind = Some(addr.to_string());
		}

		let mut workers = Vec::with_capacity(count as usize);
		let mut certificates = None;
		let mut addr: Option<SocketAddr> = None;

		for index in 0..count {
			// `max(1)` because an empty core list means pinning is off, not that
			// there are no workers.
			let core = cores.get(index as usize % cores.len().max(1)).copied();
			let shard = Shard::new(index, count).expect("index is below count");

			let worker = Worker::spawn(listen.clone(), quic.clone(), shard, core)?;

			// The group only balances a port every member actually holds. An
			// ephemeral bind gives each worker a port of its own instead, leaving all
			// but the first unreachable behind an address that looks bound.
			match addr {
				Some(first) if first != worker.addr => {
					return Err(Error::WorkerPortMismatch {
						index,
						addr: worker.addr,
						first,
					});
				}
				Some(_) => {}
				None => addr = Some(worker.addr),
			}

			certificates.get_or_insert_with(|| {
				worker
					.server
					.as_ref()
					.expect("a freshly bound worker holds its server")
					.certificates()
			});
			workers.push(worker);
		}

		let addr = addr.expect("at least one worker");
		tracing::info!(workers = count, pinned = !cores.is_empty(), %addr, "bound QUIC workers");

		Ok(Self {
			workers,
			// Every worker loads the same certificate files, so any one of them
			// answers for the fingerprint endpoint.
			certificates: certificates.expect("at least one worker"),
			addr,
		})
	}

	/// The address every worker is bound to.
	pub fn local_addr(&self) -> SocketAddr {
		self.addr
	}

	/// The certificates the workers are serving, tracking hot reloads.
	///
	/// Each worker watches the files itself, so a rotation is not atomic across
	/// the group: for as long as the reloads take, two workers can be serving
	/// different certificates.
	pub fn certificates(&self) -> crate::tls::Certificates {
		self.certificates.clone()
	}

	/// How many workers share the port.
	pub fn len(&self) -> usize {
		self.workers.len()
	}

	/// Always false: a bound group has at least one member.
	pub fn is_empty(&self) -> bool {
		self.workers.is_empty()
	}

	/// Each worker's bound server, paired with a handle onto the thread that has
	/// to drive it.
	///
	/// Split rather than served directly because only the caller knows what to do
	/// with an accepted connection, and a worker only pays off if that work runs
	/// on its own thread: build a future from the [`Server`] and hand it to
	/// [`Spawner::run`].
	///
	/// The spawners borrow the group rather than owning their threads, so the
	/// threads are released together, by [`Workers::shutdown`] or by dropping the
	/// group. Empty after the first call, since a worker serves one server.
	///
	/// # Run every server, and stop the group when one stops
	///
	/// Each [`Server`] owns its socket, and a socket leaving a reuseport group is
	/// not a local event: the kernel moves the last socket into the vacated slot,
	/// so connection IDs encoding the moved member now steer out of range and fall
	/// back to the address hash. Live sessions on a worker that is still healthy
	/// break.
	///
	/// So dropping one of these servers, or letting the future built from it
	/// return while its siblings serve, corrupts steering for the rest. Run all of
	/// them, and treat the first one that finishes as the end of the group. The
	/// type system does not enforce this yet, because doing so means handing the
	/// group a closure to build each future from, and this crate keeps callbacks
	/// out of its public API. Tracked in
	/// <https://github.com/moq-dev/moq/issues/2964>.
	pub fn split(&mut self) -> Vec<(Server, Spawner<'_>)> {
		self.workers
			.iter_mut()
			.filter_map(|worker| {
				let server = worker.server.take()?;
				Some((
					server,
					Spawner {
						index: worker.index,
						handle: &worker.handle,
					},
				))
			})
			.collect()
	}

	/// Stop every worker and wait for its threads, off the caller's runtime.
	///
	/// Dropping the group does the same thing, but joins on the calling thread,
	/// which inside an async task is a shared executor thread. Prefer this
	/// wherever there is a runtime to hand the joins to.
	pub async fn shutdown(mut self) {
		let threads = self.signal();
		if threads.is_empty() {
			return;
		}

		// Joining blocks, so it goes to the pool that exists for that rather than
		// stalling whichever executor thread happened to run this.
		let _ = tokio::task::spawn_blocking(move || join(threads)).await;
	}

	/// Tell every worker to stop and take its thread, leaving nothing for a later
	/// [`Self::shutdown`] or [`Drop`] to join twice.
	fn signal(&mut self) -> Vec<(u16, std::thread::JoinHandle<()>)> {
		// Every signal first, then the joins: a worker cannot exit while it still
		// holds a live sender, so joining as we go would serialize the teardown.
		for worker in &mut self.workers {
			worker.stop.take();
		}

		self.workers
			.iter_mut()
			.filter_map(|worker| Some((worker.index, worker.thread.take()?)))
			.collect()
	}
}

impl Drop for Workers {
	/// Stop every worker and wait for its thread, so the sockets are released
	/// before the group is gone.
	///
	/// Detaching instead would leave threads accepting into a cluster their owner
	/// has finished with, and a replacement group would join the same reuseport
	/// group as the orphans and lose a share of its traffic to them. A no-op after
	/// [`Self::shutdown`], which is the same teardown without the blocking join.
	fn drop(&mut self) {
		join(self.signal());
	}
}

/// Wait for every worker thread, reporting the ones that panicked.
fn join(threads: Vec<(u16, std::thread::JoinHandle<()>)>) {
	for (index, thread) in threads {
		if thread.join().is_err() {
			tracing::error!(index, "QUIC worker panicked");
		}
	}
}

/// Spawns onto one worker's thread, for as long as its group is alive.
///
/// The lifetime is the point: it ties what a caller runs to the [`Workers`] that
/// owns the thread, so the group is still what starts and stops every member.
#[derive(Clone, Copy, Debug)]
pub struct Spawner<'a> {
	index: u16,
	handle: &'a tokio::runtime::Handle,
}

impl Spawner<'_> {
	/// Drive `future` on this worker's thread, where its QUIC driver lives.
	///
	/// The returned handle reports what the future returned, so a caller can end
	/// the process on a worker that fails. Dropping the handle does *not* stop the
	/// future, since it runs on a thread of its own; stopping the group does.
	pub fn run<F>(&self, future: F) -> tokio::task::JoinHandle<F::Output>
	where
		F: Future + Send + 'static,
		F::Output: Send + 'static,
	{
		self.handle.spawn(future)
	}

	/// This worker's position in the group, from zero.
	pub fn index(&self) -> u16 {
		self.index
	}
}

/// One worker: a bound [`Server`] and the thread that has to drive it.
///
/// Only ever reached through [`Workers::split`], and never owned by the caller.
/// A member that could be dropped on its own would leave the group: the kernel
/// numbers a reuseport group by position and moves the last socket into any slot
/// a `close` vacates, so one member leaving renumbers a sibling out from under
/// every connection ID already issued against it.
struct Worker {
	server: Option<Server>,
	index: u16,
	handle: tokio::runtime::Handle,
	thread: Option<std::thread::JoinHandle<()>>,
	stop: Option<tokio::sync::oneshot::Sender<()>>,
	addr: SocketAddr,
}

impl Worker {
	/// Bind this worker's socket on a thread of its own, returning once it is
	/// listening.
	fn spawn(
		listen: crate::listen::Config,
		quic: crate::quic::Config,
		shard: Shard,
		core: Option<CoreId>,
	) -> Result<Self> {
		let index = shard.index();
		let (ready_tx, ready_rx) = std::sync::mpsc::channel();
		let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();

		let thread = std::thread::Builder::new()
			.name(format!("moq-quic-{index}"))
			.spawn(move || run(shard, core, listen, quic, ready_tx, stop_rx))
			.map_err(|err| Error::WorkerStart {
				index,
				source: Arc::new(err),
			})?;

		// A worker that fails to bind drops its sender, so a recv error and a bind
		// error are the same event; report the bind error when there is one.
		let ready = match ready_rx.recv() {
			Ok(ready) => ready,
			Err(_) => {
				let _ = thread.join();
				return Err(Error::WorkerStart {
					index,
					source: Arc::new(std::io::Error::other("worker exited before binding")),
				});
			}
		};

		let Ready { server, addr, handle } = match ready {
			Ok(ready) => ready,
			Err(err) => {
				// The thread is already unwinding to its end; join it so the failure
				// leaves nothing behind.
				let _ = thread.join();
				return Err(err);
			}
		};

		tracing::debug!(index, %addr, "QUIC worker listening");

		Ok(Self {
			server: Some(server),
			index,
			handle,
			thread: Some(thread),
			stop: Some(stop_tx),
			addr,
		})
	}
}

impl Drop for Worker {
	/// Stop this member and wait for its thread, so it is fully gone before its
	/// owner moves on.
	///
	/// A whole group is torn down by [`Workers`], which takes the threads first
	/// and leaves this a no-op. What this covers is partial construction: a
	/// [`Workers::bind`] that fails midway drops the members it already spawned,
	/// and without the join here its error would return while their sockets were
	/// still closing, so an immediate rebind of the same address could join the
	/// half-dead reuseport group and be renumbered when it finished dying.
	fn drop(&mut self) {
		self.stop.take();
		if let Some(thread) = self.thread.take()
			&& thread.join().is_err()
		{
			tracing::error!(index = self.index, "QUIC worker panicked");
		}
	}
}

/// What a worker reports back once it is listening.
struct Ready {
	server: Server,
	addr: SocketAddr,
	handle: tokio::runtime::Handle,
}

/// One worker thread: pin, bind, report, then park until it is stopped.
///
/// The runtime is built and entered here, and the [`Server`] is constructed
/// inside it on purpose: the QUIC backend spawns its socket driver where it is
/// built, which is what keeps this worker's packets on this thread. The server
/// is handed back for the owner to build an accept loop from, which
/// [`Spawner::run`] spawns onto this same runtime.
fn run(
	shard: Shard,
	core: Option<CoreId>,
	listen: crate::listen::Config,
	quic: crate::quic::Config,
	ready: std::sync::mpsc::Sender<Result<Ready>>,
	stop: tokio::sync::oneshot::Receiver<()>,
) {
	let index = shard.index();
	if let Some(core) = core {
		pin(index, core);
	}

	let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
		Ok(runtime) => runtime,
		Err(err) => {
			let _ = ready.send(Err(Error::WorkerStart {
				index,
				source: Arc::new(err),
			}));
			return;
		}
	};

	let built = {
		let _guard = runtime.enter();
		Server::build(listen, quic, crate::server::Parts::Shard(shard))
			.and_then(|server| server.local_addr().map(|addr| (server, addr)))
	};

	let (server, addr) = match built {
		Ok(built) => built,
		Err(err) => {
			let _ = ready.send(Err(err));
			return;
		}
	};

	if ready
		.send(Ok(Ready {
			server,
			addr,
			handle: runtime.handle().clone(),
		}))
		.is_err()
	{
		return;
	}

	// Parked, not idle: the runtime has to keep turning for the endpoint driver
	// and whatever the owner spawned. `stop` resolves when its sender drops.
	runtime.block_on(async move {
		let _ = stop.await;
	});
	tracing::debug!(index, "QUIC worker stopped");
}

/// A core to pin a worker to. Kept behind a type so the non-Linux build has
/// something to name without depending on the pinning crate's types.
type CoreId = core_affinity::CoreId;

/// The cores workers may be pinned to, in the order they are handed out.
///
/// Empty when the platform will not report them, which disables pinning rather
/// than failing: the mode's other half (one runtime and one socket per worker)
/// is still worth having.
fn cores() -> Vec<CoreId> {
	let cores = core_affinity::get_core_ids().unwrap_or_default();
	if cores.is_empty() {
		tracing::warn!("could not enumerate CPU cores; QUIC workers will not be pinned");
	}
	cores
}

/// Pin the calling thread to `core`, warning if the platform refuses.
fn pin(index: u16, core: CoreId) {
	if core_affinity::set_for_current(core) {
		tracing::debug!(index, core = core.id, "pinned QUIC worker");
	} else {
		tracing::warn!(index, core = core.id, "failed to pin QUIC worker");
	}
}
