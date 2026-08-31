//! On-demand jemalloc heap profiling, for chasing leaks in a running process.
//!
//! Spawn [`run`] to dump a profile whenever the process gets SIGUSR1.

use tikv_jemalloc_ctl::raw;

pub use tikv_jemallocator;

async fn blocking<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, tokio::task::JoinError> {
	tokio::task::spawn_blocking(work).await
}

fn register_signal() -> std::io::Result<tokio::signal::unix::Signal> {
	tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())
}

/// Listen for SIGUSR1 and dump a jemalloc heap profile on each signal.
///
/// Profiling must be enabled at startup via `MALLOC_CONF=prof:true`
/// (and typically `prof_active:true` plus a `prof_prefix`). jemalloc
/// only initializes the profiling backend when `opt.prof` is set at
/// init; toggling `prof.active` later returns EINVAL.
pub async fn run() -> crate::Result<()> {
	// Register before inspecting jemalloc. SIGUSR1's default action is process
	// termination, so even a build or configuration without profiling must own it.
	let mut sig = register_signal()?;

	let active = match unsafe { raw::read::<bool>(b"prof.active\0") } {
		Ok(true) => {
			tracing::info!("jemalloc heap profiling is active");
			true
		}
		Ok(false) => {
			tracing::info!(
				"jemalloc profiling compiled in but not active. Set MALLOC_CONF=prof:true,prof_active:true at startup to enable"
			);
			false
		}
		Err(err) => {
			tracing::debug!(%err, "jemalloc profiling not available");
			false
		}
	};

	loop {
		sig.recv().await;
		if !active {
			tracing::warn!("heap profile requested while jemalloc profiling is inactive");
			continue;
		}

		// Null pointer tells jemalloc to use prof_prefix from MALLOC_CONF.
		match blocking(|| unsafe { raw::write(b"prof.dump\0", std::ptr::null::<u8>()) }).await {
			Ok(Ok(())) => tracing::info!("heap profile dumped"),
			Ok(Err(err)) => tracing::error!(%err, "failed to dump heap profile"),
			Err(err) => tracing::error!(%err, "heap profile task failed"),
		}
	}
}

#[cfg(test)]
mod tests {
	#[tokio::test(flavor = "current_thread")]
	async fn blocking_work_leaves_the_runtime_thread() {
		let runtime = std::thread::current().id();
		let worker = super::blocking(|| std::thread::current().id()).await.unwrap();
		assert_ne!(runtime, worker);
	}

	#[tokio::test]
	async fn usr1_is_caught_without_active_profiling() {
		let mut signal = super::register_signal().unwrap();
		unsafe { libc::raise(libc::SIGUSR1) };
		tokio::time::timeout(std::time::Duration::from_secs(1), signal.recv())
			.await
			.unwrap();
	}
}
