//! On-demand jemalloc heap profiling, for chasing leaks in a running process.
//!
//! Spawn [`run`] to dump a profile whenever the process gets SIGUSR1.
//!
//! Sampling does not have to be running for that to work. `MALLOC_CONF=prof:true` alone builds
//! the profiling backend without sampling anything, and the first SIGUSR1 starts it. That keeps
//! the steady-state cost off a process nobody is profiling: sampling accrues per-allocation
//! metadata for as long as it runs, which on a busy process is hundreds of megabytes.

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

/// Whether jemalloc built its profiling backend, which is what makes `prof.active` writable.
///
/// Fixed at startup by `MALLOC_CONF=prof:true`; there is no way to turn it on later, which is the
/// one thing that genuinely has to be decided before the process starts.
fn backend_ready() -> bool {
	matches!(unsafe { raw::read::<bool>(b"opt.prof\0") }, Ok(true))
}

/// Whether sampling is running right now. Unlike `opt.prof`, this is writable at any time.
fn sampling() -> bool {
	matches!(unsafe { raw::read::<bool>(b"prof.active\0") }, Ok(true))
}

/// Listen for SIGUSR1 and dump a jemalloc heap profile on each signal.
///
/// Requires `MALLOC_CONF=prof:true` at startup (plus a `prof_prefix` to choose where dumps land).
/// `prof_active:true` is optional: without it the first signal starts sampling, so a process only
/// pays for profiling once someone asks for it.
pub async fn run() -> crate::Result<()> {
	// Register before inspecting jemalloc. SIGUSR1's default action is process
	// termination, so even a build or configuration without profiling must own it.
	let mut sig = register_signal()?;
	#[cfg(test)]
	signal_ready().notify_one();

	let ready = backend_ready();
	if !ready {
		tracing::debug!("jemalloc profiling not available. Set MALLOC_CONF=prof:true at startup to enable");
	} else if sampling() {
		tracing::info!("jemalloc heap profiling is active");
	} else {
		tracing::info!("jemalloc heap profiling is idle; the first SIGUSR1 starts sampling");
	}

	loop {
		sig.recv().await;
		if !ready {
			tracing::warn!(
				"heap profile requested, but jemalloc profiling was not enabled at startup (MALLOC_CONF=prof:true)"
			);
			continue;
		}

		// Start sampling on the first request rather than at startup, so an unprofiled process
		// carries none of the cost. Only allocations from here on are sampled: whatever the heap
		// already holds is not attributed retroactively, so a dump taken to explain an
		// already-large heap wants `prof_active:true` from the start instead.
		if !sampling() {
			match blocking(|| unsafe { raw::write(b"prof.active\0", true) }).await {
				Ok(Ok(())) => {
					tracing::info!("jemalloc heap profiling started; this dump covers only allocations from now on")
				}
				Ok(Err(err)) => {
					tracing::error!(%err, "failed to start heap profiling");
					continue;
				}
				Err(err) => {
					tracing::error!(%err, "start-profiling task failed");
					continue;
				}
			}
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

	/// The point of the on-demand path: a process started with the backend built but sampling off
	/// pays nothing until someone signals it, and the signal is what turns sampling on.
	#[test]
	fn usr1_starts_idle_sampling() {
		let prefix = std::env::temp_dir().join(format!("moq-jemalloc-test-{}", std::process::id()));
		let output = std::process::Command::new(std::env::current_exe().unwrap())
			.args(["--exact", "jemalloc::tests::usr1_child_starts_sampling"])
			.env(USR1_CHILD, "1")
			.env(
				"MALLOC_CONF",
				format!("prof:true,prof_active:false,prof_prefix:{}", prefix.display()),
			)
			.output()
			.unwrap();

		// The child dumps once it has started sampling; clean up whatever it wrote.
		if let Some(dir) = prefix.parent() {
			let name = prefix.file_name().unwrap().to_string_lossy().into_owned();
			if let Ok(entries) = std::fs::read_dir(dir) {
				for entry in entries.flatten() {
					if entry.file_name().to_string_lossy().starts_with(&name) {
						let _ = std::fs::remove_file(entry.path());
					}
				}
			}
		}

		assert!(
			output.status.success(),
			"child did not start sampling on SIGUSR1:\n{}",
			String::from_utf8_lossy(&output.stderr)
		);
	}

	#[test]
	fn usr1_child_starts_sampling() {
		if std::env::var_os(USR1_CHILD).is_none() {
			return;
		}

		// Nothing to switch on where jemalloc was built without the profiling backend: `prof:true`
		// is ignored there, and this test has no subject.
		if !super::backend_ready() {
			return;
		}
		assert!(!super::sampling(), "prof_active:false should leave sampling off");

		tokio::runtime::Builder::new_current_thread()
			.enable_all()
			.build()
			.unwrap()
			.block_on(async {
				let runner = tokio::spawn(super::run());
				super::signal_ready().notified().await;
				unsafe { libc::raise(libc::SIGUSR1) };

				// Flipping `prof.active` hops to a blocking thread, so poll rather than assume it
				// has landed by the next yield.
				for _ in 0..200 {
					if super::sampling() {
						break;
					}
					tokio::time::sleep(std::time::Duration::from_millis(10)).await;
				}

				assert!(super::sampling(), "SIGUSR1 did not start sampling");
				assert!(!runner.is_finished());
				runner.abort();
			});
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
