//! Thread-per-core QUIC workers.
//!
//! By default the relay runs one work-stealing runtime and one UDP socket:
//! every packet can cross threads, and every wakeup is a candidate context
//! switch. [`RuntimeConfig::workers`] switches QUIC to the opposite shape.
//! Each worker is a thread of its own, pinned to a core, running a
//! `current_thread` runtime and owning one socket in a `SO_REUSEPORT` group. A
//! connection lands on whichever worker the kernel steers its first packet to
//! and stays there: its QUIC driver, its session, and its tasks never leave
//! that thread, so the locks they take are uncontended and nothing is stolen.
//!
//! Everything that is not QUIC (the web and internal listeners, `tcp`/`unix`,
//! clustering, signals) stays on the main runtime. The workers reach the rest
//! of the relay through the shared [`Cluster`], the same way sessions on one
//! runtime already do.
//!
//! # TLS
//!
//! Each worker builds its own listener, so each loads the certificate files
//! itself and watches them for reloads independently. They serve the same
//! identity, but a rotation is not atomic across the group: for as long as the
//! reloads take, two workers can be serving different certificates, and a
//! worker whose watcher failed to start keeps serving the old one. The
//! fingerprint published at `/certificate.sha256` is the first worker's, which
//! is why a generated certificate is refused here (see [`Workers::bind`]).
//!
//! # Steering
//!
//! Left alone, the kernel picks a member of a reuseport group by hashing the
//! packet's 4-tuple, which is the wrong key for QUIC: a client that changes
//! address would hash somewhere else and its packets would arrive at a worker
//! that has never heard of the connection. So the group is steered by
//! connection ID instead (`moq_tokio`'s `steer` module): each worker issues
//! connection IDs that name it, and a filter on the group reads them back. A
//! connection follows its ID, not its address, which is what QUIC means by
//! migration.
//!
//! That is also why this needs a backend whose connection IDs we can choose.
//! Quinn and noq allow it; the quiche backend does not, and refuses to start
//! with workers rather than silently falling back to address hashing.

use std::net::SocketAddr;

use anyhow::Context;
use clap::Args;
use serde::{Deserialize, Serialize};

use crate::{Auth, Cluster, Config, Shutdown};

/// How the relay lays its QUIC work out over threads.
#[derive(Args, Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
#[group(id = "runtime-config")]
pub struct RuntimeConfig {
	/// Serve QUIC from this many single-threaded workers instead of the shared
	/// runtime, each pinned to a core with its own socket on the listen address.
	///
	/// A connection is handled start to finish by one worker, which trades the
	/// shared runtime's load balancing for no cross-thread traffic per packet.
	/// The group is steered by connection ID, so a client that changes address
	/// stays with its worker.
	///
	/// Linux-only, and needs a backend whose connection IDs can carry the
	/// worker's identity (quinn or noq, not quiche). The listen address needs an
	/// explicit non-zero port, and `--listen-tls-generate` is refused because
	/// each worker would generate a certificate of its own. Unset (the default)
	/// keeps QUIC on the shared runtime.
	#[arg(long = "runtime-workers", env = "MOQ_RUNTIME_WORKERS")]
	pub workers: Option<u16>,

	/// Pin each worker to a CPU core, defaulting to on.
	///
	/// Pinning is the point of the mode: it keeps a connection's caches warm on
	/// one core and stops the scheduler migrating a busy worker. Turn it off to
	/// measure what pinning alone is worth, or when sharing the machine with
	/// something that manages CPU placement itself.
	#[arg(long = "runtime-pin", env = "MOQ_RUNTIME_PIN")]
	pub pin: Option<bool>,
}

/// The QUIC workers, bound and waiting to serve.
///
/// [`Workers::bind`] opens every worker's socket, so a returned value is
/// listening; nothing is accepted until [`Workers::serve`]. Inert (and cheap)
/// when [`RuntimeConfig::workers`] is unset, which is how [`crate::Relay`] can
/// hold one unconditionally.
pub struct Workers {
	workers: Vec<Worker>,
	certificates: Option<moq_tokio::tls::Certificates>,
	addr: Option<SocketAddr>,
}

/// One worker thread and the three channels that own its lifetime.
///
/// Each is an `Option` because [`Workers`] hands them out or drops them at
/// different points, and the worker reads a dropped sender as an instruction:
/// no [`Self::start`] means never serve, no [`Self::stop`] means stop serving.
struct Worker {
	shard: u16,

	/// Joined by [`Workers::drop`]. Held rather than detached so the socket is
	/// provably released before the pool is gone, instead of by a thread that
	/// outlives it.
	thread: std::thread::JoinHandle<()>,

	/// Sends the worker the cluster to accept into. Dropping it instead tells a
	/// worker that never got started to exit.
	start: Option<tokio::sync::oneshot::Sender<Start>>,

	/// Never sent on: dropping it is the stop signal, which is what makes the
	/// accept loop cancellable from outside the worker's own runtime.
	stop: Option<tokio::sync::oneshot::Sender<()>>,

	/// The result of the worker's accept loop.
	done: Option<tokio::sync::oneshot::Receiver<anyhow::Result<()>>>,
}

/// What a worker needs to start accepting, known only after the cluster exists.
struct Start {
	cluster: Cluster,
	auth: Auth,
	shutdown: Shutdown,
}

impl Workers {
	/// Bind one socket per worker, or nothing at all when workers are disabled.
	///
	/// Returns once every worker is listening, so a port conflict or a bad
	/// certificate is an error here rather than a worker that quietly died.
	pub fn bind(config: &Config) -> anyhow::Result<Self> {
		let Some(count) = config.runtime.workers.filter(|count| *count > 0) else {
			return Ok(Self::disabled());
		};

		anyhow::ensure!(
			config.listen.tls.generate.is_empty(),
			"--runtime-workers cannot be combined with --listen-tls-generate: \
			 each worker would generate a different certificate. Pass --listen-tls-cert/--listen-tls-key instead."
		);

		let pin = config.runtime.pin.unwrap_or(true).then(cores).unwrap_or_default();

		let mut workers = Vec::with_capacity(count as usize);
		let mut certificates = None;
		let mut addr: Option<SocketAddr> = None;

		for shard in 0..count {
			let listen = shard_config(config, shard, count)?;
			let quic = config.quic.clone();
			let core = pin.get(shard as usize % pin.len().max(1)).copied();

			let (ready_tx, ready_rx) = std::sync::mpsc::channel();
			let (start_tx, start_rx) = tokio::sync::oneshot::channel();
			let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
			let (done_tx, done_rx) = tokio::sync::oneshot::channel();

			// Held, not detached: `Workers` is what releases these sockets, and it
			// can only promise that if it can join the threads holding them.
			let thread = std::thread::Builder::new()
				.name(format!("moq-quic-{shard}"))
				.spawn(move || run(shard, core, listen, quic, ready_tx, start_rx, stop_rx, done_tx))
				.with_context(|| format!("failed to spawn QUIC worker {shard}"))?;

			// A worker that fails to bind drops its sender, so the recv error and
			// the bind error are the same event; report the bind error.
			let ready = ready_rx
				.recv()
				.map_err(|_| anyhow::anyhow!("QUIC worker {shard} exited before binding"))?
				.with_context(|| format!("QUIC worker {shard} failed to listen"))?;

			// The group only balances a port every member actually holds. An
			// ephemeral bind gives each worker a port of its own instead, leaving
			// all but the first unreachable behind an address that looks bound.
			match addr {
				Some(first) if first != ready.addr => anyhow::bail!(
					"QUIC worker {shard} bound {} instead of {first}; \
					 every worker must share one port, so --listen needs an explicit non-zero port",
					ready.addr
				),
				Some(_) => {}
				None => addr = Some(ready.addr),
			}

			certificates.get_or_insert(ready.certificates);

			workers.push(Worker {
				shard,
				thread,
				start: Some(start_tx),
				stop: Some(stop_tx),
				done: Some(done_rx),
			});
		}

		tracing::info!(workers = count, pinned = !pin.is_empty(), "started QUIC workers");

		Ok(Self {
			workers,
			// Every worker loads the same certificate files, so any one of them
			// answers for the fingerprint endpoint.
			certificates,
			addr,
		})
	}

	/// A pool that owns nothing, for a relay serving QUIC on the shared runtime.
	fn disabled() -> Self {
		Self {
			workers: Vec::new(),
			certificates: None,
			addr: None,
		}
	}

	/// Whether QUIC is being served by workers rather than the shared runtime.
	pub fn enabled(&self) -> bool {
		!self.workers.is_empty()
	}

	/// The address every worker is bound to, or `None` when they are disabled.
	pub fn local_addr(&self) -> Option<SocketAddr> {
		self.addr
	}

	/// The certificates the workers are serving, tracking hot reloads, or `None`
	/// when they are disabled and the shared server holds them instead.
	pub fn certificates(&self) -> Option<moq_tokio::tls::Certificates> {
		self.certificates.clone()
	}

	/// Release every worker to accept into `cluster`, then resolve when one stops.
	///
	/// Pends forever when the workers are disabled, so it composes into a
	/// `select!` either way. A worker only stops on error or on shutdown, so the
	/// first one to finish ends the relay the same way the shared accept loop does.
	///
	/// Borrows rather than consuming, so the caller still holds the pool when this
	/// resolves or is cancelled. That is what makes the sockets go away: dropping
	/// the returned future stops nothing by itself, since each worker's accept loop
	/// lives on a thread of its own. Dropping the [`Workers`] is what stops them.
	pub async fn serve(&mut self, cluster: Cluster, auth: Auth, shutdown: Shutdown) -> anyhow::Result<()> {
		if self.workers.is_empty() {
			return std::future::pending().await;
		}

		let mut running = futures::stream::FuturesUnordered::new();
		for worker in &mut self.workers {
			let start = Start {
				cluster: cluster.clone(),
				auth: auth.clone(),
				shutdown: shutdown.clone(),
			};
			let shard = worker.shard;
			// The worker is parked on this receiver; it only drops if the thread
			// died, which its `done` half reports with the real error.
			if let Some(sender) = worker.start.take() {
				let _ = sender.send(start);
			}
			let done = worker.done.take();
			running.push(async move {
				match done {
					Some(done) => match done.await {
						Ok(res) => res.with_context(|| format!("QUIC worker {shard} failed")),
						Err(_) => Err(anyhow::anyhow!("QUIC worker {shard} exited without reporting")),
					},
					// Already awaited by an earlier `serve`, so this worker has
					// nothing left to report; leave the outcome to its siblings.
					None => std::future::pending().await,
				}
			});
		}

		use futures::StreamExt;
		running
			.next()
			.await
			.unwrap_or_else(|| Err(anyhow::anyhow!("no QUIC workers")))
	}
}

impl std::fmt::Debug for Workers {
	/// Hand-written because the certificate handle behind [`Workers::certificates`]
	/// is opaque, and the shard count plus the shared address is what identifies a
	/// pool anyway.
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Workers")
			.field("workers", &self.workers.len())
			.field("addr", &self.addr)
			.finish()
	}
}

impl Drop for Workers {
	/// Stop every worker and wait for its thread, so the sockets are released
	/// before the pool is gone.
	///
	/// Detaching instead would leave threads accepting into a cluster their owner
	/// has finished with, and a replacement pool would join the same reuseport
	/// group as the orphans and lose a share of its traffic to them. Blocking here
	/// is deliberate: the stop signal goes out first, so each thread is already on
	/// its way out by the time it is joined.
	fn drop(&mut self) {
		// Every signal first, then the joins: a worker cannot exit while it still
		// holds a live sender, so joining as we go would serialize the teardown.
		for worker in &mut self.workers {
			worker.start.take();
			worker.stop.take();
		}

		for worker in self.workers.drain(..) {
			let shard = worker.shard;
			if worker.thread.join().is_err() {
				tracing::error!(shard, "QUIC worker panicked");
			}
		}
	}
}

/// What a worker reports back once it is listening.
struct Ready {
	certificates: moq_tokio::tls::Certificates,
	addr: SocketAddr,
}

/// The listen config for one worker: QUIC only, on its own socket in the group.
fn shard_config(config: &Config, index: u16, count: u16) -> anyhow::Result<moq_tokio::listen::Config> {
	let mut listen = config.listen.clone();
	listen.listeners = moq_tokio::listen::Listeners::Quic;
	listen.shard = Some(
		moq_tokio::listen::Shard::new(index, count)
			.with_context(|| format!("invalid QUIC worker {index} of {count}"))?,
	);
	Ok(listen)
}

/// One worker thread: bind, report, wait for the cluster, then serve until it stops.
#[allow(clippy::too_many_arguments)]
fn run(
	shard: u16,
	core: Option<CoreId>,
	listen: moq_tokio::listen::Config,
	quic: moq_tokio::quic::Config,
	ready: std::sync::mpsc::Sender<anyhow::Result<Ready>>,
	start: tokio::sync::oneshot::Receiver<Start>,
	stop: tokio::sync::oneshot::Receiver<()>,
	done: tokio::sync::oneshot::Sender<anyhow::Result<()>>,
) {
	if let Some(core) = core {
		pin(shard, core);
	}

	let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
		Ok(runtime) => runtime,
		Err(err) => {
			let _ = ready.send(Err(anyhow::Error::new(err).context("failed to build worker runtime")));
			return;
		}
	};

	runtime.block_on(async move {
		// Built inside the runtime on purpose: the QUIC backend spawns its socket
		// driver where it is constructed, which is what keeps this worker's packets
		// on this thread.
		let server = match listen.init(quic) {
			Ok(server) => server,
			Err(err) => {
				let _ = ready.send(Err(err.into()));
				return;
			}
		};

		let addr = match server.local_addr() {
			Ok(addr) => addr,
			Err(err) => {
				let _ = ready.send(Err(err.into()));
				return;
			}
		};

		tracing::debug!(shard, %addr, "QUIC worker listening");
		if ready
			.send(Ok(Ready {
				certificates: server.certificates(),
				addr,
			}))
			.is_err()
		{
			return;
		}

		let Ok(start) = start.await else {
			return;
		};

		// `stop` resolves when its sender drops, which is the only way to get out
		// of the accept loop: it runs on this thread, so the owner cancelling its
		// own future would leave this one running.
		tokio::select! {
			res = crate::serve(server, start.cluster, start.auth, start.shutdown) => {
				let _ = done.send(res);
			}
			_ = stop => tracing::debug!(shard, "QUIC worker stopping"),
		}
	});
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
fn pin(shard: u16, core: CoreId) {
	if core_affinity::set_for_current(core) {
		tracing::debug!(shard, core = core.id, "pinned QUIC worker");
	} else {
		tracing::warn!(shard, core = core.id, "failed to pin QUIC worker");
	}
}
