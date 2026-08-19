//! Flags for serving QUIC from pinned per-core workers.
//!
//! The workers themselves are [`moq_tokio::worker`]; this is the operator-facing
//! half, which is the only part specific to the relay. Unset (the default) keeps
//! QUIC on the shared runtime with everything else.

use clap::Args;
use serde::{Deserialize, Serialize};

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

impl RuntimeConfig {
	/// The worker group this asks for, or `None` to keep QUIC on the shared
	/// runtime.
	///
	/// Zero workers reads as unset rather than as an error: it is the natural way
	/// to switch the mode off from a config file that already has the section.
	pub fn workers(&self) -> Option<moq_tokio::worker::Config> {
		let count = self.workers.filter(|count| *count > 0)?;
		Some(moq_tokio::worker::Config::new(count).with_pin(self.pin.unwrap_or(true)))
	}
}
