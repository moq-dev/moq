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

#[cfg(test)]
fn signal_ready() -> &'static tokio::sync::Notify {
	static READY: std::sync::OnceLock<tokio::sync::Notify> = std::sync::OnceLock::new();
	READY.get_or_init(tokio::sync::Notify::new)
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
	#[cfg(test)]
	signal_ready().notify_one();

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
	const USR1_CHILD: &str = "MOQ_NATIVE_USR1_CHILD";

	#[tokio::test(flavor = "current_thread")]
	async fn blocking_work_leaves_the_runtime_thread() {
		let runtime = std::thread::current().id();
		let worker = super::blocking(|| std::thread::current().id()).await.unwrap();
		assert_ne!(runtime, worker);
	}

	#[test]
	fn usr1_is_caught_without_active_profiling() {
		let output = std::process::Command::new(std::env::current_exe().unwrap())
			.args(["--exact", "jemalloc::tests::usr1_child_survives_without_profiling"])
			.env(USR1_CHILD, "1")
			.env("MALLOC_CONF", "prof:false,prof_active:false")
			.output()
			.unwrap();
		assert!(
			output.status.success(),
			"child did not survive SIGUSR1:\n{}",
			String::from_utf8_lossy(&output.stderr)
		);
	}

	#[test]
	fn usr1_child_survives_without_profiling() {
		if std::env::var_os(USR1_CHILD).is_none() {
			return;
		}

		assert!(!matches!(
			unsafe { super::raw::read::<bool>(b"prof.active\0") },
			Ok(true)
		));
		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(async {
				let runner = tokio::spawn(super::run());
				super::signal_ready().notified().await;
				unsafe { libc::raise(libc::SIGUSR1) };
				tokio::task::yield_now().await;
				assert!(!runner.is_finished());
				runner.abort();
			});
	}
}
