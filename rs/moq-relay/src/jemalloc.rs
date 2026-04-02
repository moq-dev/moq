use tikv_jemalloc_ctl::raw;

/// Activate jemalloc heap profiling and listen for SIGUSR1 to dump profiles.
///
/// The dump path is controlled by `MALLOC_CONF=prof_prefix:<path>`.
/// Returns `pending` if profiling is not available (i.e. MALLOC_CONF=prof:true was not set).
pub async fn run() -> anyhow::Result<()> {
	let prof_active = b"prof.active\0";

	match unsafe { raw::read::<bool>(prof_active) } {
		Ok(true) => tracing::info!("jemalloc heap profiling is active"),
		Ok(false) => {
			tracing::info!("jemalloc profiling compiled in; activating");
			unsafe { raw::write(prof_active, true) }.ok();
		}
		Err(err) => {
			tracing::warn!(%err, "jemalloc profiling not available — set MALLOC_CONF=prof:true to enable");
			return std::future::pending().await;
		}
	}

	let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;

	loop {
		sig.recv().await;

		// Empty path tells jemalloc to use prof_prefix from MALLOC_CONF.
		match unsafe { raw::write(b"prof.dump\0", b"\0" as *const u8) } {
			Ok(()) => tracing::info!("heap profile dumped"),
			Err(err) => tracing::error!(%err, "failed to dump heap profile"),
		}
	}
}
