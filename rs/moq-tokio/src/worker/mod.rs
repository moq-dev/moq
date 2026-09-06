//! Thread-per-core QUIC workers.
//!
//! A `Server` on a work-stealing runtime serves every connection off one UDP
//! socket: every packet can cross threads, and every wakeup is a candidate
//! context switch. `Workers` is the opposite shape. Each
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
//! [`Config`] is just the shape of a group, so it compiles wherever the crate
//! does: a caller may be handing the same count and pinning to a runtime of its
//! own. The group itself binds QUIC sockets, so it needs a QUIC backend and is
//! absent without one.

// Everything but the knobs: a group binds and serves QUIC, so it needs a backend
// to bind with.
#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
mod group;

#[cfg(any(feature = "noq", feature = "quinn", feature = "quiche"))]
pub use group::{Spawner, Workers};

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
