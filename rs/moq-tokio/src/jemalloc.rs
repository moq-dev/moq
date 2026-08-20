//! On-demand jemalloc heap profiling, for chasing leaks in a running process.
//!
//! Spawn [`run`] to dump a profile whenever the process gets SIGUSR1.

use tikv_jemalloc_ctl::raw;

pub use tikv_jemallocator;

/// Listen for SIGUSR1 and dump a jemalloc heap profile on each signal.
///
/// Profiling must be enabled at startup via `MALLOC_CONF=prof:true`
/// (and typically `prof_active:true` plus a `prof_prefix`). jemalloc
/// only initializes the profiling backend when `opt.prof` is set at
/// init; toggling `prof.active` later returns EINVAL.
pub async fn run() -> crate::Result<()> {
	match unsafe { raw::read::<bool>(b"prof.active\0") } {
		Ok(true) => tracing::info!("jemalloc heap profiling is active"),
		Ok(false) => {
			tracing::info!(
				"jemalloc profiling compiled in but not active. Set MALLOC_CONF=prof:true,prof_active:true at startup to enable"
			);
			return Ok(());
		}
		Err(err) => {
			tracing::debug!(%err, "jemalloc profiling not available");
			return Ok(());
		}
	}

	let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;

	loop {
		sig.recv().await;

		// Null pointer tells jemalloc to use prof_prefix from MALLOC_CONF.
		match unsafe { raw::write(b"prof.dump\0", std::ptr::null::<u8>()) } {
			Ok(()) => tracing::info!("heap profile dumped"),
			Err(err) => tracing::error!(%err, "failed to dump heap profile"),
		}
	}
}
