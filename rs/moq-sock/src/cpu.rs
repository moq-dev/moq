//! Pinning worker threads to cores.

/// A core a worker thread can be pinned to.
///
/// Opaque on purpose: obtained from [`cores`] and handed back to [`pin`], so
/// which crate does the pinning stays this module's business rather than a
/// breaking change waiting on somebody else's release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CoreId(core_affinity::CoreId);

impl CoreId {
	/// The operating system's number for this core, for logging.
	pub fn id(self) -> usize {
		self.0.id
	}
}

impl std::fmt::Display for CoreId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0.id)
	}
}

/// The cores workers may be pinned to, in the order they are handed out.
///
/// Empty when the platform will not report them, which callers should treat
/// as pinning being off rather than an error: a thread-per-core listener's
/// other half (one runtime and one socket per worker) is still worth having.
pub fn cores() -> Vec<CoreId> {
	let cores = core_affinity::get_core_ids().unwrap_or_default();
	if cores.is_empty() {
		tracing::warn!("could not enumerate CPU cores; workers will not be pinned");
	}
	cores.into_iter().map(CoreId).collect()
}

/// Pin the calling thread to `core`. Returns whether the platform obliged;
/// a refusal is worth a warning, not a failure.
pub fn pin(core: CoreId) -> bool {
	core_affinity::set_for_current(core.0)
}
