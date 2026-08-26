//! Pinning worker threads to cores.
//!
//! A thin wrapper over [`core_affinity`]; [`CoreId`] is its type, re-exported,
//! so a major bump of that crate is a breaking change here.

/// A core a worker thread can be pinned to.
pub type CoreId = core_affinity::CoreId;

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
	cores
}

/// Pin the calling thread to `core`. Returns whether the platform obliged;
/// a refusal is worth a warning, not a failure.
pub fn pin(core: CoreId) -> bool {
	core_affinity::set_for_current(core)
}
