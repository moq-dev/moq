use std::future::Future;
use std::net::SocketAddr;
use std::sync::{
	Arc,
	atomic::{AtomicBool, Ordering},
};

use super::Config;
use crate::{Error, Result, Server, abort::AbortOnDrop, listen::Member, server::SocketRetainer};

/// A bound group of QUIC workers sharing one port.
///
/// [`Workers::bind`] opens every socket, so a returned value is already
/// listening and a port conflict is an error here rather than a worker that
/// quietly died. Nothing is accepted until [`Workers::split`] hands the group
/// to the caller and each member is served from its own thread.
///
/// ```no_run
/// # async fn example(listen: moq_tokio::listen::Config) -> anyhow::Result<()> {
/// use moq_tokio::worker;
///
/// let workers = worker::Workers::bind(listen, Default::default(), worker::Config::new(8))?;
/// println!("listening on {}", workers.local_addr());
///
/// // The group owns the threads, so keep it alive as long as you want the
/// // port served.
/// let mut group = workers.split();
/// let mut tasks = Vec::new();
/// for (server, spawner) in group.members() {
///     tasks.push(spawner.serve(server, |server| async move {
///         let _ = server.listen().await;
///     }));
/// }
/// group.shutdown().await;
/// # Ok(())
/// # }
/// ```
pub struct Workers {
	workers: Vec<Worker>,
	certificates: crate::tls::Certificates,
	addr: SocketAddr,

	/// The reuseport group the workers bound into. Held for the group's
	/// lifetime, because it is what holds the listen port against a second
	/// group.
	_group: moq_sock::shard::Group,
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

		let cores = config.pin.then(cores).unwrap_or_default();

		// One resolution for the whole group. Each worker resolves its own config
		// otherwise, so a DNS answer that rotates between queries would hand
		// members different addresses and fail the bind on whichever member drew a
		// fresh one. An unset `bind` stays unset: the backends fall back to the
		// default literal, and `Some` here would flip a stream-only config into
		// opening a QUIC listener.
		let mut listen = listen;
		let requested = match listen.bind.as_deref() {
			Some(bind) => {
				let addr = crate::util::resolve(Some(bind), bind).map_err(|err| Error::WorkerResolve(Arc::new(err)))?;
				listen.bind = Some(addr.to_string());
				addr
			}
			None => crate::server::DEFAULT_BIND
				.parse()
				.expect("the default bind is a literal"),
		};

		// The group owns everything a reuseport group has to get right: it takes
		// the port before the first member binds and holds it until the group is
		// dropped, refuses a size the steering filter could not address, and
		// hands out one member per slot in the order the kernel numbers them by.
		let mut group = moq_sock::shard::Group::acquire(requested, config.count).map_err(|err| match err {
			moq_sock::shard::Error::Count { count, max } => Error::WorkerCount { count, max },
			// The port lock is the only other way to lose the address, and it is
			// held by exactly one thing: another group of this UID.
			_ => Error::WorkerOverlap { addr: requested },
		})?;
		let count = group.count();

		let shared = Arc::new(Shared::default());

		let mut workers = Vec::with_capacity(count as usize);
		let mut certificates = None;

		while let Some(member) = group.member() {
			let index = member.shard().index();
			// `max(1)` because an empty core list means pinning is off, not that
			// there are no workers.
			let core = cores.get(index as usize % cores.len().max(1)).copied();

			let worker = Worker::spawn(listen.clone(), quic.clone(), member, core, shared.clone())?;

			certificates.get_or_insert_with(|| {
				worker
					.server
					.as_ref()
					.expect("a freshly bound worker holds its server")
					.certificates()
			});
			workers.push(worker);
		}

		// Whatever the first member bound, which is the requested address unless
		// it asked for an ephemeral port.
		let addr = group.addr();
		tracing::info!(workers = count, pinned = !cores.is_empty(), %addr, "bound QUIC workers");

		Ok(Self {
			workers,
			// Every worker loads the same certificate files, so any one of them
			// answers for the fingerprint endpoint.
			certificates: certificates.expect("at least one worker"),
			addr,
			_group: group,
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

	/// Hand the bound workers to an owning group that serves them together.
	///
	/// Consuming, so a worker serves one server: there is no second split that
	/// hands the same socket out twice. The returned group owns every thread
	/// and retains every socket until serving has stopped, so dropping a
	/// returned server or letting one member's future return cannot resize the
	/// reuseport array out from under its siblings. Serve each member with
	/// [`Spawner::serve`]: the first member to complete, fail, or cancel ends
	/// serving for the group.
	pub fn split(self) -> Group {
		let shared = match self.workers.first() {
			Some(worker) => worker.shared.clone(),
			None => Arc::new(Shared::default()),
		};

		// Retain every socket before handing any server out: dropping a server
		// afterwards closes its endpoint, but the retainer keeps the socket in
		// the group, so the kernel never renumbers a survivor.
		let mut retainers = Vec::with_capacity(self.workers.len());
		for worker in &self.workers {
			retainers.push(
				worker
					.server
					.as_ref()
					.expect("a bound worker holds its server before the split")
					.retain(),
			);
		}
		*shared.retainers.lock().unwrap() = retainers;

		Group {
			workers: self.workers,
			certificates: self.certificates,
			addr: self.addr,
			_group: self._group,
			shared,
		}
	}

	/// Stop every worker and wait for its threads, off the caller's runtime.
	///
	/// For a bound group that was never split: drops the servers with the
	/// group and releases the port. Prefer splitting and shutting down the
	/// [`Group`] once serving has started.
	pub async fn shutdown(mut self) {
		let threads = signal_workers(&mut self.workers);
		if threads.is_empty() {
			return;
		}

		// Joining blocks, so it goes to the pool that exists for that rather than
		// stalling whichever executor thread happened to run this.
		let _ = tokio::task::spawn_blocking(move || join(threads)).await;
	}
}

/// An owning group of bound QUIC workers, serving one port together.
///
/// Returned by [`Workers::split`], which consumes the bound workers so each
/// socket is handed out once. The group owns every worker thread and retains
/// every member socket until serving has stopped: dropping a [`Server`] handle
/// or letting one serving future return cannot take one socket out of the
/// reuseport group while its siblings keep serving.
///
/// Serve each member with [`Spawner::serve`]. The first member whose serving
/// future completes, panics, or is cancelled ends serving for the group: its
/// siblings are stopped and the group is ready to join. Join with
/// [`Group::shutdown`] (off the caller's runtime) or by dropping the group.
pub struct Group {
	workers: Vec<Worker>,
	certificates: crate::tls::Certificates,
	addr: SocketAddr,
	shared: Arc<Shared>,

	/// The reuseport group the workers bound into. Held for the group's
	/// lifetime, because it is what holds the listen port against a second
	/// group.
	_group: moq_sock::shard::Group,
}

impl std::fmt::Debug for Group {
	/// Hand-written because the bound []s are opaque; the member count and
	/// the shared address is what identifies a group anyway.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Group")
			.field("workers", &self.workers.len())
			.field("addr", &self.addr)
			.finish()
	}
}

impl Group {
	/// The address every worker is bound to.
	pub fn local_addr(&self) -> SocketAddr {
		self.addr
	}

	/// The certificates the workers are serving, tracking hot reloads.
	///
	/// See [`Workers::certificates`]: a rotation is not atomic across members.
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

	/// Each worker's bound server, paired with a handle onto the thread that
	/// has to drive it.
	///
	/// Serve each pair with [`Spawner::serve`], not [`Spawner::run`]: `serve`
	/// builds the future on the worker's thread from the server and ends the
	/// group when the first member finishes, while `run` is for auxiliary
	/// tasks that must not end serving. Empty after the first call, since a
	/// worker serves one server.
	///
	/// Dropping a returned server is safe but leaves its slot unserved: the
	/// group retains the socket, so the survivors keep their steering, but
	/// nothing accepts the dropped member's share until the group stops.
	pub fn members(&mut self) -> Vec<(Server, Spawner<'_>)> {
		self.workers
			.iter_mut()
			.filter_map(|worker| {
				let server = worker.server.take()?;
				Some((
					server,
					Spawner {
						index: worker.index,
						handle: &worker.handle,
						spawn: &worker.spawn,
						shared: &worker.shared,
					},
				))
			})
			.collect()
	}

	/// Resolve once any serving member has ended the group.
	///
	/// The output itself travels through the [`Spawner::serve`] handle that ran
	/// it; this is the owner's signal that serving is over and
	/// [`Group::shutdown`] will join cleanly. Pends forever while every member
	/// still serves.
	pub async fn finished(&self) {
		self.shared.stopped().await;
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
		signal_workers(&mut self.workers)
	}
}

impl Drop for Group {
	/// Stop every worker and wait for its thread, so the sockets are released
	/// before the group is gone.
	///
	/// Detaching instead would leave threads accepting into a cluster their owner
	/// has finished with, and a replacement group would join the same reuseport
	/// group as the orphans and lose a share of its traffic to them. A no-op after
	/// [`Self::shutdown`], which is the same teardown without the blocking join.
	///
	/// Never joins the calling thread: dropping the group from one of its own
	/// worker tasks detaches that thread instead of deadlocking on it.
	fn drop(&mut self) {
		join(self.signal());
	}
}

/// Tell every worker in `workers` to stop and take its thread.
///
/// Shared by the bound [`Workers`] (never served) and the serving [`Group`]:
/// both own the same threads, and a worker cannot exit while it still holds a
/// live stop sender, so every signal goes out before any join.
fn signal_workers(workers: &mut [Worker]) -> Vec<(u16, std::thread::JoinHandle<()>)> {
	if let Some(shared) = workers.first().map(|worker| worker.shared.clone()) {
		shared.cancel();
	}

	// Every signal first, then the joins: a worker cannot exit while it still
	// holds a live sender, so joining as we go would serialize the teardown.
	for worker in workers.iter_mut() {
		worker.stop.take();
	}

	workers
		.iter_mut()
		.filter_map(|worker| Some((worker.index, worker.thread.take()?)))
		.collect()
}

/// Wait for every worker thread, reporting the ones that panicked.
///
/// Skips the calling thread, which can only happen when the group is dropped
/// from one of its own worker tasks: joining it would deadlock, so it is
/// detached to exit on its own once the stop signal lands.
fn join(threads: Vec<(u16, std::thread::JoinHandle<()>)>) {
	let current = std::thread::current().id();
	for (index, thread) in threads {
		// A handle for the calling thread means this runs on a worker, so
		// drop it to detach instead of deadlocking on a self-join. Forgetting
		// the handle would leave a joinable thread's OS resources behind.
		if thread.thread().id() == current {
			tracing::warn!(index, "QUIC worker group dropped on its own thread; detaching it");
			continue;
		}
		if thread.join().is_err() {
			tracing::error!(index, "QUIC worker panicked");
		}
	}
}

/// What the workers share: the shutdown signal and the retained sockets.
///
/// One per group, held by the group and borrowed by every spawner. Cancelling
/// it stops every worker thread; the retainers keep every socket in the
/// reuseport group until the group is gone.
#[derive(Debug, Default)]
struct Shared {
	shutdown: AtomicBool,
	notify: tokio::sync::Notify,
	retainers: std::sync::Mutex<Vec<SocketRetainer>>,
}

impl Shared {
	/// Stop every worker watching this group. Idempotent: the first member to
	/// end serving and an explicit shutdown race here on every teardown.
	fn cancel(&self) {
		if !self.shutdown.swap(true, Ordering::SeqCst) {
			self.notify.notify_waiters();
		}
	}

	/// Resolve once [`cancel`](Self::cancel) has run, whenever that was.
	async fn stopped(&self) {
		loop {
			// Subscribe before reading the flag: a cancel between a false load
			// and `notified()` would call `notify_waiters` with no waiter and
			// this future would sleep through an already-true shutdown.
			let notified = self.notify.notified();
			if self.shutdown.load(Ordering::SeqCst) {
				return;
			}
			notified.await;
		}
	}
}

/// Cancels the shared group shutdown when the serving future ends.
///
/// Held inside the forwarding future [`Spawner::serve`] spawns, so every way
/// the member ends (return, panic, or abort) stops its siblings. Dropping the
/// outer handle without aborting leaves the task running, so it does not stop
/// the group on its own.
struct CancelOnDrop(Arc<Shared>);

impl Drop for CancelOnDrop {
	fn drop(&mut self) {
		self.0.cancel();
	}
}

/// Spawns onto one worker's thread, for as long as its group is alive.
///
/// The lifetime is the point: it ties what a caller runs to the [`Group`] that
/// owns the thread, so the group is still what starts and stops every member.
#[derive(Clone, Copy, Debug)]
pub struct Spawner<'a> {
	index: u16,
	handle: &'a tokio::runtime::Handle,
	spawn: &'a tokio::sync::mpsc::UnboundedSender<Spawn>,
	shared: &'a Arc<Shared>,
}

impl Spawner<'_> {
	/// Serve this worker's server, ending the group when the future ends.
	///
	/// The factory crosses the thread boundary with the server, then creates
	/// the future inside the worker's local task set, so the future itself may
	/// be `!Send`. It is a builder rather than a hook: it runs once, to make
	/// the future, and nothing calls back into it afterwards.
	///
	/// Completion, panic, or cancellation of the future stops every sibling:
	/// the group owns termination, so a member that returns while its siblings
	/// serve is the end of serving, not a resized reuseport group. The returned
	/// handle reports what the future returned; dropping it without aborting
	/// leaves the task running.
	pub fn serve<M, F>(&self, server: Server, make: M) -> tokio::task::JoinHandle<F::Output>
	where
		M: FnOnce(Server) -> F + Send + 'static,
		F: Future + 'static,
		F::Output: Send + 'static,
	{
		let shared = self.shared.clone();
		let (task_tx, task_rx) = tokio::sync::oneshot::channel();
		let spawn = Box::new(move || {
			// The factory runs inside the task, not as the argument to the spawn:
			// panicking while building the future is then the task's panic, not the
			// worker thread's.
			let task = AbortOnDrop::new(tokio::task::spawn_local(async move { make(server).await }));
			// The guard travels with the handle, so every way of losing it from here
			// on (a failed send, a caller that aborts while it is still in flight,
			// the forwarding task below being cancelled) cancels the task instead of
			// detaching it onto the worker.
			let _ = task_tx.send(task);
		});
		let _ = self.spawn.send(spawn);

		// Ending for any reason ends the group. Captured (not created inside
		// the body) so an abort before the first poll still stops the
		// siblings: dropping an unpolled future drops its captures without
		// running any of the body that would have created the guard.
		let cancel = CancelOnDrop(shared.clone());
		self.handle.spawn(async move {
			let _cancel = cancel;
			let mut task = task_rx.await.expect("worker stopped before spawning task");
			match (&mut *task).await {
				Ok(output) => output,
				Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
				Err(err) => panic!("worker-local task cancelled: {err}"),
			}
		})
	}

	/// Build and drive a future on this worker's thread, where its QUIC driver lives.
	///
	/// The factory crosses the thread boundary, then creates the future inside
	/// the worker's local task set, so the future itself may be `!Send`. It is a
	/// builder rather than a hook: it runs once, to make the future, and nothing
	/// calls back into it afterwards.
	///
	/// Auxiliary tasks only: unlike [`serve`](Self::serve) this does not own a
	/// server and ending it does not end the group. The returned handle reports
	/// what the future returned, so a caller can end the process on a worker
	/// that fails, and stands in for the worker-local task itself: a panic in
	/// the factory or the future surfaces through it, and
	/// [`abort`](tokio::task::JoinHandle::abort) cancels the task on the worker.
	/// Dropping the handle does *not* stop the future, since it runs on a thread
	/// of its own; stopping the group does.
	pub fn run<M, F>(&self, make: M) -> tokio::task::JoinHandle<F::Output>
	where
		M: FnOnce() -> F + Send + 'static,
		F: Future + 'static,
		F::Output: Send + 'static,
	{
		let (task_tx, task_rx) = tokio::sync::oneshot::channel();
		let spawn = Box::new(move || {
			// The factory runs inside the task, not as the argument to the spawn:
			// panicking while building the future is then the task's panic, not the
			// worker thread's.
			let task = AbortOnDrop::new(tokio::task::spawn_local(async move { make().await }));
			// The guard travels with the handle, so every way of losing it from here
			// on (a failed send, a caller that aborts while it is still in flight,
			// the forwarding task below being cancelled) cancels the task instead of
			// detaching it onto the worker.
			let _ = task_tx.send(task);
		});
		let _ = self.spawn.send(spawn);

		self.handle.spawn(async move {
			let mut task = task_rx.await.expect("worker stopped before spawning task");
			match (&mut *task).await {
				Ok(output) => output,
				Err(err) if err.is_panic() => std::panic::resume_unwind(err.into_panic()),
				Err(err) => panic!("worker-local task cancelled: {err}"),
			}
		})
	}

	/// This worker's position in the group, from zero.
	pub fn index(&self) -> u16 {
		self.index
	}
}

/// One worker: a bound [`Server`] and the thread that has to drive it.
///
/// Only ever reached through [`Group::members`], and never owned by the caller
/// alone: the group retains every socket, so a member that is dropped on its
/// own cannot renumber a survivor out from under its connection IDs.
struct Worker {
	server: Option<Server>,
	index: u16,
	handle: tokio::runtime::Handle,
	spawn: tokio::sync::mpsc::UnboundedSender<Spawn>,
	thread: Option<std::thread::JoinHandle<()>>,
	stop: Option<tokio::sync::oneshot::Sender<()>>,
	shared: Arc<Shared>,
}

impl Worker {
	/// Bind this worker's socket on a thread of its own, returning once it is
	/// listening.
	fn spawn(
		listen: crate::listen::Config,
		quic: crate::quic::Config,
		member: Member,
		core: Option<CoreId>,
		shared: Arc<Shared>,
	) -> Result<Self> {
		let index = member.shard().index();
		let (ready_tx, ready_rx) = std::sync::mpsc::channel();
		let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
		let (spawn_tx, spawn_rx) = tokio::sync::mpsc::unbounded_channel();

		let thread = std::thread::Builder::new()
			.name(format!("moq-quic-{index}"))
			.spawn({
				let shared = shared.clone();
				move || run(member, core, listen, quic, ready_tx, stop_rx, spawn_rx, shared)
			})
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
			spawn: spawn_tx,
			thread: Some(thread),
			stop: Some(stop_tx),
			shared,
		})
	}
}

impl Drop for Worker {
	/// Stop this member and wait for its thread, so it is fully gone before its
	/// owner moves on.
	///
	/// A whole group is torn down by [`Group`], which takes the threads first
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

/// A future factory that is sent to and invoked by its worker thread.
type Spawn = Box<dyn FnOnce() + Send + 'static>;

/// One worker thread: pin, bind, report, then park until it is stopped.
///
/// The runtime is built and entered here, and the [`Server`] is constructed
/// inside it on purpose: the QUIC backend spawns its socket driver where it is
/// built, which is what keeps this worker's packets on this thread. The server
/// is handed back for the owner to build an accept loop from, which
/// [`Spawner::serve`] spawns onto this same runtime.
#[allow(clippy::too_many_arguments)]
fn run(
	member: Member,
	core: Option<CoreId>,
	listen: crate::listen::Config,
	quic: crate::quic::Config,
	ready: std::sync::mpsc::Sender<Result<Ready>>,
	stop: tokio::sync::oneshot::Receiver<()>,
	mut spawn: tokio::sync::mpsc::UnboundedReceiver<Spawn>,
	shared: Arc<Shared>,
) {
	let index = member.shard().index();
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
		Server::build(
			crate::server::Config::default().with_listen(listen).with_quic(quic),
			crate::server::Parts::Member(member),
		)
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
	// and whatever the owner spawned. `stop` resolves when its sender drops, and
	// the shared shutdown fires when any serving member ends the group.
	let local = tokio::task::LocalSet::new();
	local.block_on(&runtime, async move {
		tokio::pin!(stop);
		loop {
			tokio::select! {
				_ = &mut stop => break,
				_ = shared.stopped() => break,
				Some(spawn) = spawn.recv() => spawn(),
			}
		}
	});
	tracing::debug!(index, "QUIC worker stopped");
}

/// A core to pin a worker to.
type CoreId = moq_sock::cpu::CoreId;

/// The cores workers may be pinned to; empty disables pinning with a warning.
fn cores() -> Vec<CoreId> {
	moq_sock::cpu::cores()
}

/// Pin the calling thread to `core`, warning if the platform refuses.
fn pin(index: u16, core: CoreId) {
	if moq_sock::cpu::pin(core) {
		tracing::debug!(index, core = core.id(), "pinned QUIC worker");
	} else {
		tracing::warn!(index, core = core.id(), "failed to pin QUIC worker");
	}
}
