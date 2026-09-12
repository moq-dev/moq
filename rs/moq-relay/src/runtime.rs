//! Flags for serving QUIC from pinned per-core workers.
//!
//! The workers themselves are [`moq_tokio::worker`]; this is the operator-facing
//! half, which is the only part specific to the relay. Unset (the default) keeps
//! QUIC on the shared runtime with everything else.

use serde::{Deserialize, Serialize};

/// How the relay lays its QUIC work out over threads.
#[derive(usage::Args, Clone, Debug, Deserialize, Serialize)]
#[usage(unknown_flags = "error", args_override_self = false)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct RuntimeConfig {
	/// Serve QUIC from this many single-threaded workers instead of the shared
	/// runtime, each pinned to a core with its own socket on the listen address.
	///
	/// A connection is handled start to finish by one worker, which trades the
	/// shared runtime's load balancing for no cross-thread traffic per packet.
	/// Linux-only, and mutually exclusive with `--listen-tls-generate`, since
	/// each worker would otherwise generate and serve a certificate of its own.
	/// Unset (the default) keeps QUIC on the shared runtime.
	#[usage(long = "runtime-workers", env = "MOQ_RUNTIME_WORKERS", setting = "runtime.workers")]
	pub workers: Option<u16>,

	/// Pin each worker to a CPU core, defaulting to on.
	///
	/// The mode's measured win comes from each worker owning a socket and a
	/// runtime, not from pinning: on a single-socket machine, pinned and unpinned
	/// benchmark inside run-to-run noise of each other. Pinning stops the
	/// scheduler migrating a busy worker, which should matter on a multi-socket
	/// or NUMA machine, and costs nothing elsewhere. Turn it off when sharing
	/// the machine with something that manages CPU placement itself.
	#[usage(
		long = "runtime-pin",
		env = "MOQ_RUNTIME_PIN",
		setting = "runtime.pin",
		default = "true",
		bool_value
	)]
	pub pin: bool,

	/// Drive the QUIC workers with io_uring instead of tokio.
	///
	/// Each worker owns a `SINGLE_ISSUER` ring, batched UDP (multishot receive
	/// with `UDP_GRO`, `UDP_SEGMENT` send), userspace timers, and a local task
	/// set; sessions are moq-lite only (native raw QUIC and browser
	/// WebTransport alike), and everything else (HTTP, WebSocket, cluster
	/// dials, stats) stays on the shared runtime. Requires `runtime-workers`,
	/// Linux 6.12+, and refuses to start anywhere it cannot deliver.
	#[usage(
		long = "runtime-io-uring",
		env = "MOQ_RUNTIME_IO_URING",
		setting = "runtime.io_uring",
		bool_value
	)]
	pub io_uring: bool,
}

impl RuntimeConfig {
	/// The worker group this asks for, or `None` to keep QUIC on the shared
	/// runtime.
	///
	/// Zero workers reads as unset rather than as an error: it is the natural way
	/// to switch the mode off from a config file that already has the section.
	pub fn workers(&self) -> Option<moq_tokio::worker::Config> {
		let count = self.workers.filter(|count| *count > 0)?;
		Some(moq_tokio::worker::Config::new(count).with_pin(self.pin))
	}

	/// Whether the workers should run on io_uring instead of tokio.
	pub fn io_uring(&self) -> bool {
		self.io_uring
	}
}

impl Default for RuntimeConfig {
	fn default() -> Self {
		Self {
			workers: None,
			pin: true,
			io_uring: false,
		}
	}
}
