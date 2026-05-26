use tikv_jemalloc_ctl::raw;

pub use tikv_jemallocator;

/// Activate jemalloc heap profiling and listen for SIGUSR1 to dump profiles.
///
/// The dump path is controlled by `MALLOC_CONF=prof_prefix:<path>`.
/// Profiling is a debug aid, so any setup failure is logged and swallowed
/// rather than propagated. Returns when profiling can't be set up.
pub async fn run() -> anyhow::Result<()> {
	let prof_active = b"prof.active\0";

	match unsafe { raw::read::<bool>(prof_active) } {
		Ok(true) => tracing::info!("jemalloc heap profiling is active"),
		Ok(false) => {
			tracing::info!("jemalloc profiling compiled in; activating");
			if let Err(err) = unsafe { raw::write(prof_active, true) } {
				tracing::warn!(%err, "failed to activate jemalloc profiling, continuing without it");
				return Ok(());
			}
		}
		Err(err) => {
			tracing::debug!(%err, "jemalloc profiling not available. Set MALLOC_CONF=prof:true to enable");
			return Ok(());
		}
	}

	let mut sig = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1()) {
		Ok(sig) => sig,
		Err(err) => {
			tracing::warn!(%err, "failed to install SIGUSR1 handler for jemalloc profile dumps");
			return Ok(());
		}
	};

	loop {
		sig.recv().await;

		// Null pointer tells jemalloc to use prof_prefix from MALLOC_CONF.
		match unsafe { raw::write(b"prof.dump\0", std::ptr::null::<u8>()) } {
			Ok(()) => tracing::info!("heap profile dumped"),
			Err(err) => tracing::warn!(%err, "failed to dump heap profile"),
		}
	}
}
