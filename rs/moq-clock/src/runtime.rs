//! The runtime backend: tokio's clock on native (paused tokio included), and
//! `performance.now()` with `setTimeout` in the browser, both through
//! `web_async::time`. The only module, with the simulator, that reads a host clock.
#![allow(clippy::disallowed_methods)]

use std::{
	future::Future,
	pin::Pin,
	sync::OnceLock,
	task::{Context, Poll},
};

use web_async::time as host;

use crate::{Instant, Wall};

/// Where every unpaused clock counts from, so instants read on any thread, in any
/// runtime or none, stay comparable.
static ORIGIN: OnceLock<host::Instant> = OnceLock::new();

/// The origin this context counts from, and a reading of its clock.
///
/// The origin is a real instant captured before the reading, so an unpaused clock
/// never reads below it. A paused tokio runtime's clock starts at the runtime's
/// build, which can precede the origin; that runtime counts from its own first
/// reading instead (see [`paused`]).
fn read() -> (host::Instant, host::Instant) {
	#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
	let origin = *ORIGIN.get_or_init(|| host::Instant::from_std(std::time::Instant::now()));
	let reading = host::Instant::now();
	// The browser has one clock and no paused runtimes.
	#[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
	let origin = *ORIGIN.get_or_init(|| reading);

	#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
	let origin = paused::origin(origin, reading);
	(origin, reading)
}

pub(crate) fn now() -> Instant {
	let (origin, reading) = read();
	Instant::from_origin(reading.duration_since(origin))
}

pub(crate) fn wall() -> Wall {
	let since = host::SystemTime::now()
		.duration_since(host::UNIX_EPOCH)
		.expect("the host wall clock reads before 1970");
	Wall::UNIX_EPOCH + since
}

/// A host timer for `at`, on the origin it was created with.
pub(crate) struct Timer {
	origin: host::Instant,
	sleep: Pin<Box<host::Sleep>>,
}

impl Timer {
	pub(crate) fn new(at: Instant) -> Self {
		let (origin, _) = read();
		Self {
			origin,
			sleep: Box::pin(host::sleep_until(origin + at.since_origin())),
		}
	}

	pub(crate) fn reset(&mut self, at: Instant) {
		self.sleep.as_mut().reset(self.origin + at.since_origin());
	}

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		// Checked first, so a reached deadline fires at once rather than at the
		// host timer's next tick.
		if host::Instant::now() >= self.sleep.deadline() {
			return Poll::Ready(());
		}
		self.sleep.as_mut().poll(cx)
	}
}

#[cfg(any(not(target_arch = "wasm32"), target_os = "wasi"))]
mod paused {
	use std::{
		collections::HashMap,
		sync::{
			LazyLock, Mutex,
			atomic::{AtomicBool, Ordering},
		},
	};

	use super::host;

	/// Paused runtimes whose clock started before [`ORIGIN`](super::ORIGIN), with the
	/// first reading each counts from. Only test runtimes land here: an unpaused
	/// clock never reads below the origin. Never pruned; one entry per such runtime.
	static OWN: LazyLock<Mutex<HashMap<tokio::runtime::Id, host::Instant>>> = LazyLock::new(Default::default);
	/// Whether [`OWN`] has any entry, so production reads skip its lock.
	static ANY: AtomicBool = AtomicBool::new(false);

	pub(super) fn origin(origin: host::Instant, reading: host::Instant) -> host::Instant {
		if reading >= origin && !ANY.load(Ordering::Acquire) {
			return origin;
		}
		let Ok(runtime) = tokio::runtime::Handle::try_current() else {
			assert!(reading >= origin, "a clock outside any runtime read before the origin");
			return origin;
		};
		let mut own = OWN.lock().unwrap();
		if let Some(own) = own.get(&runtime.id()) {
			return *own;
		}
		if reading >= origin {
			return origin;
		}
		own.insert(runtime.id(), reading);
		ANY.store(true, Ordering::Release);
		reading
	}
}

#[cfg(all(test, any(not(target_arch = "wasm32"), target_os = "wasi")))]
mod tests {
	use std::time::Duration;

	/// Paused tokio time moves `now()` and nothing else does, so measurements
	/// beside a tokio sleep agree with it.
	#[test]
	fn paused_tokio_drives_the_clock() {
		// Built before anything reads the clock, so in a fresh process its paused
		// clock starts before the origin and it counts from its own.
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.start_paused(true)
			.build()
			.unwrap();
		let _ = crate::now();
		runtime.block_on(async {
			let start = crate::now();
			crate::sleep(Duration::from_secs(5)).await;
			assert_eq!(crate::now(), start + Duration::from_secs(5));
			tokio::time::advance(Duration::from_secs(60)).await;
			assert_eq!(crate::now(), start + Duration::from_secs(65));
			let at = crate::sleep_until(start + Duration::from_secs(70));
			assert_eq!(at.deadline(), start + Duration::from_secs(70));
			at.await;
			assert_eq!(crate::now(), start + Duration::from_secs(70));
		});
	}

	/// Every worker of a multi-thread runtime, and a thread outside any runtime,
	/// count from one origin.
	#[test]
	fn threads_share_one_origin() {
		let runtime = tokio::runtime::Builder::new_multi_thread()
			.worker_threads(4)
			.enable_time()
			.build()
			.unwrap();
		let before = crate::now();
		let reads = runtime.block_on(async {
			let tasks: Vec<_> = (0..16).map(|_| tokio::spawn(async { crate::now() })).collect();
			let mut reads = Vec::new();
			for task in tasks {
				reads.push(task.await.unwrap());
			}
			reads
		});
		let after = crate::now();
		for read in reads {
			assert!(before <= read && read <= after, "{before:?} <= {read:?} <= {after:?}");
		}
	}
}
