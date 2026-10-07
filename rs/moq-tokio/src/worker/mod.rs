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
#[cfg(feature = "noq")]
mod group;

#[cfg(feature = "noq")]
pub use group::{Group, Member, Spawner, Workers};

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

/// How long one QUIC worker thread has blocked on a [`kio::Lock`].
///
/// The thread binds it for its whole life. Clones share the counters, which
/// is how a scrape reads them from off the thread.
#[derive(Clone, Debug, Default)]
pub struct LockWait {
	inner: kio::LockWait,
}

impl LockWait {
	/// Time blocked, and how many acquires blocked.
	pub fn snapshot(&self) -> (std::time::Duration, u64) {
		self.inner.snapshot()
	}

	pub(crate) fn bind(&self) -> kio::LockWaitBind {
		self.inner.bind()
	}
}

/// One contended `kio` lock call site, summed across every thread.
#[derive(Clone, Copy, Debug)]
pub struct LockSite {
	/// Source file of the `lock` call.
	pub file: &'static str,
	/// Line of the `lock` call.
	pub line: u32,
	/// Column of the `lock` call.
	pub column: u32,
	/// Time blocked at this call site.
	pub wait: std::time::Duration,
	/// How many acquires at this call site blocked.
	pub contended: u64,
}

/// Contended acquires since process start, hottest call site first.
pub fn lock_sites() -> Vec<LockSite> {
	kio::lock_sites()
		.into_iter()
		.map(|site| LockSite {
			file: site.file,
			line: site.line,
			column: site.column,
			wait: site.wait,
			contended: site.contended,
		})
		.collect()
}

/// Contended acquires that missed the fixed site table.
pub fn lock_site_overflow() -> u64 {
	kio::lock_site_overflow()
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
