use tikv_jemalloc_ctl::raw;

/// Activate jemalloc heap profiling and spawn a SIGUSR1 signal handler to dump profiles.
///
/// Send `kill -USR1 <pid>` to write a heap profile to `/tmp/moq-relay.heap.<seq>`.
pub fn init() {
	let prof_active = b"prof.active\0";

	match unsafe { raw::read::<bool>(prof_active) } {
		Ok(true) => tracing::info!("jemalloc heap profiling is active"),
		Ok(false) => {
			tracing::info!("jemalloc profiling compiled in; activating");
			unsafe { raw::write(prof_active, true) }.ok();
		}
		Err(err) => {
			tracing::warn!(%err, "jemalloc profiling not available — set MALLOC_CONF=prof:true to enable");
			return;
		}
	}

	tokio::spawn(async {
		if let Err(err) = signal_handler().await {
			tracing::error!(%err, "jemalloc signal handler failed");
		}
	});
}

async fn signal_handler() -> anyhow::Result<()> {
	let mut sig = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
	let mut seq = 0u64;

	loop {
		sig.recv().await;
		let path = format!("/tmp/moq-relay.heap.{seq}\0");
		seq += 1;

		match unsafe { raw::write(b"prof.dump\0", path.as_ptr()) } {
			Ok(()) => tracing::info!(path = &path[..path.len() - 1], "heap profile dumped"),
			Err(err) => tracing::error!(%err, "failed to dump heap profile"),
		}
	}
}
