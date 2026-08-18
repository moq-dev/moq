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
//! clustering, signals, cert reload) stays on the main runtime. The workers
//! reach the rest of the relay through the shared [`Cluster`], the same way
//! sessions on one runtime already do.
//!
//! # Steering
//!
//! The kernel picks a member of the reuseport group by hashing the packet's
//! 4-tuple, which pins a connection to one worker for as long as the client
//! keeps its address. A client that migrates (a NAT rebinding, a network
//! change) hashes somewhere else and its packets reach a worker that has never
//! seen the connection ID, so the connection is lost rather than migrated.
//! Steering by connection ID instead is the next piece of the work; until then
//! this mode is for measurement and for deployments where that trade is
//! acceptable, which is why it is off by default.

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
	/// Linux-only, and mutually exclusive with `--listen-tls-generate`, since
	/// each worker would otherwise generate and serve a certificate of its own.
	/// Unset (the default) keeps QUIC on the shared runtime.
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
	bound: Vec<Bound>,
	certificates: Option<moq_tokio::tls::Certificates>,
	addr: Option<SocketAddr>,
}

/// One worker thread, bound and parked until it is handed the cluster to serve.
struct Bound {
	shard: u16,
	start: tokio::sync::oneshot::Sender<Start>,
	done: tokio::sync::oneshot::Receiver<anyhow::Result<()>>,
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

		let mut bound = Vec::with_capacity(count as usize);
		let mut certificates = None;
		let mut addr = None;

		for shard in 0..count {
			let listen = shard_config(config, shard, count)?;
			let quic = config.quic.clone();
			let core = pin.get(shard as usize % pin.len().max(1)).copied();

			let (ready_tx, ready_rx) = std::sync::mpsc::channel();
			let (start_tx, start_rx) = tokio::sync::oneshot::channel();
			let (done_tx, done_rx) = tokio::sync::oneshot::channel();

			std::thread::Builder::new()
				.name(format!("moq-quic-{shard}"))
				.spawn(move || run(shard, core, listen, quic, ready_tx, start_rx, done_tx))
				.with_context(|| format!("failed to spawn QUIC worker {shard}"))?;

			// A worker that fails to bind drops its sender, so the recv error and
			// the bind error are the same event; report the bind error.
			let ready = ready_rx
				.recv()
				.map_err(|_| anyhow::anyhow!("QUIC worker {shard} exited before binding"))?
				.with_context(|| format!("QUIC worker {shard} failed to listen"))?;

			certificates.get_or_insert(ready.certificates);
			addr.get_or_insert(ready.addr);

			bound.push(Bound {
				shard,
				start: start_tx,
				done: done_rx,
			});
		}

		tracing::info!(workers = count, pinned = !pin.is_empty(), "started QUIC workers");

		Ok(Self {
			bound,
			// Every worker loads the same certificate files, so any one of them
			// answers for the fingerprint endpoint.
			certificates,
			addr,
		})
	}

	/// A pool that owns nothing, for a relay serving QUIC on the shared runtime.
	fn disabled() -> Self {
		Self {
			bound: Vec::new(),
			certificates: None,
			addr: None,
		}
	}

	/// Whether QUIC is being served by workers rather than the shared runtime.
	pub fn enabled(&self) -> bool {
		!self.bound.is_empty()
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
	pub async fn serve(self, cluster: Cluster, auth: Auth, shutdown: Shutdown) -> anyhow::Result<()> {
		if self.bound.is_empty() {
			return std::future::pending().await;
		}

		let mut running = futures::stream::FuturesUnordered::new();
		for worker in self.bound {
			let start = Start {
				cluster: cluster.clone(),
				auth: auth.clone(),
				shutdown: shutdown.clone(),
			};
			let shard = worker.shard;
			// The worker is parked on this receiver; it only drops if the thread
			// died, which its `done` half reports with the real error.
			let _ = worker.start.send(start);
			running.push(async move {
				match worker.done.await {
					Ok(res) => res.with_context(|| format!("QUIC worker {shard} failed")),
					Err(_) => Err(anyhow::anyhow!("QUIC worker {shard} exited without reporting")),
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
fn run(
	shard: u16,
	core: Option<CoreId>,
	listen: moq_tokio::listen::Config,
	quic: moq_tokio::quic::Config,
	ready: std::sync::mpsc::Sender<anyhow::Result<Ready>>,
	start: tokio::sync::oneshot::Receiver<Start>,
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
		let _ = done.send(crate::serve(server, start.cluster, start.auth, start.shutdown).await);
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
