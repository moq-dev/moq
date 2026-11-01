//! The runtime backend: tokio's clock, paused tokio included. The only module, with
//! the simulator, that reads a host clock.
//!
//! Without the `runtime` feature there are no timers, and reads fall back to std.
#![expect(clippy::disallowed_methods, reason = "this backend is where the host clock is read")]

#[cfg(feature = "runtime")]
use std::{future::Future, pin::Pin};
use std::{
	sync::OnceLock,
	task::{Context, Poll},
};

#[cfg(feature = "runtime")]
use tokio::time as host;

use crate::{Instant, Wall};

/// How far past [`Instant::ZERO`] a runtime clock reads at its origin.
///
/// Like std's since-boot `Instant`, so `now() - d` names a moment before the
/// process started instead of underflowing during its first seconds. 2^32 s
/// (~136 years) outlasts any lookback, and leaves ~448 years of range ahead.
const LEAD: std::time::Duration = std::time::Duration::from_secs(1 << 32);

/// Where every unpaused clock counts from, so instants read on any thread, in any
/// runtime or none, stay comparable.
#[cfg(feature = "runtime")]
static ORIGIN: OnceLock<host::Instant> = OnceLock::new();
#[cfg(feature = "runtime")]
static ANCHOR: OnceLock<(Instant, Wall)> = OnceLock::new();

/// The origin this context counts from, and a reading of its clock.
///
/// The origin is a real instant captured before the reading, so an unpaused clock
/// never reads below it. A paused tokio runtime's clock starts at the runtime's
/// build, which can precede the origin; that runtime counts from its own first
/// reading instead (see [`paused`]).
#[cfg(feature = "runtime")]
fn read() -> (host::Instant, host::Instant) {
	let origin = *ORIGIN.get_or_init(|| host::Instant::from_std(std::time::Instant::now()));
	let reading = host::Instant::now();
	let origin = paused::origin(origin, reading);
	(origin, reading)
}

#[cfg(feature = "runtime")]
pub(crate) fn now() -> Instant {
	let (origin, reading) = read();
	Instant::from_origin(LEAD + reading.duration_since(origin))
}

pub(crate) fn wall() -> Wall {
	let since = std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.expect("the host wall clock reads before 1970");
	Wall::UNIX_EPOCH + since
}

#[cfg(feature = "runtime")]
pub(crate) fn anchor() -> (Instant, Wall) {
	if let Some(anchor) = paused::anchor() {
		return anchor;
	}
	*ANCHOR.get_or_init(|| (real_now(), wall()))
}

/// [`now`] from the host's real clock, so an advanced paused runtime sharing the
/// origin cannot skew the anchor every unpaused caller maps through.
#[cfg(feature = "runtime")]
fn real_now() -> Instant {
	let origin = ORIGIN.get_or_init(|| host::Instant::from_std(std::time::Instant::now()));
	Instant::from_origin(LEAD + std::time::Instant::now().saturating_duration_since(origin.into_std()))
}

/// The std instant the runtime clock counts from, as [`read`] picks it.
#[cfg(feature = "runtime")]
fn std_base() -> std::time::Instant {
	read().0.into_std()
}

#[cfg(not(feature = "runtime"))]
fn std_base() -> std::time::Instant {
	std_origin().0
}

/// `at` on std's clock, through the origin the runtime clock counts from.
pub(crate) fn into_std(at: Instant) -> Option<std::time::Instant> {
	let base = std_base();
	match at.since_origin().checked_sub(LEAD) {
		Some(after) => base.checked_add(after),
		None => base.checked_sub(LEAD - at.since_origin()),
	}
}

/// The inverse of [`into_std`], saturating at the clock's range.
pub(crate) fn from_std(at: std::time::Instant) -> Instant {
	let base = std_base();
	let lead = Instant::from_origin(LEAD);
	match at.checked_duration_since(base) {
		Some(after) => lead.checked_add(after).unwrap_or(Instant::MAX),
		None => lead.checked_sub(base - at).unwrap_or(Instant::ZERO),
	}
}

/// A host timer for `at`, on the origin it was created with.
#[cfg(feature = "runtime")]
pub(crate) struct Timer {
	origin: host::Instant,
	sleep: Pin<Box<host::Sleep>>,
}

#[cfg(feature = "runtime")]
impl Timer {
	pub(crate) fn new(at: Instant) -> Self {
		let (origin, _) = read();
		Self {
			origin,
			sleep: Box::pin(host::sleep_until(origin + at.since_origin().saturating_sub(LEAD))),
		}
	}

	pub(crate) fn reset(&mut self, at: Instant) {
		self.sleep
			.as_mut()
			.reset(self.origin + at.since_origin().saturating_sub(LEAD));
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

#[cfg(not(feature = "runtime"))]
static STD_ORIGIN: OnceLock<(std::time::Instant, Wall)> = OnceLock::new();

#[cfg(not(feature = "runtime"))]
fn std_origin() -> (std::time::Instant, Wall) {
	*STD_ORIGIN.get_or_init(|| (std::time::Instant::now(), wall()))
}

#[cfg(not(feature = "runtime"))]
pub(crate) fn now() -> Instant {
	Instant::from_origin(LEAD + std_origin().0.elapsed())
}

#[cfg(not(feature = "runtime"))]
pub(crate) fn anchor() -> (Instant, Wall) {
	(Instant::from_origin(LEAD), std_origin().1)
}

#[cfg(not(feature = "runtime"))]
pub(crate) struct Timer;

#[cfg(not(feature = "runtime"))]
impl Timer {
	pub(crate) fn new(_: Instant) -> Self {
		panic!("moq-clock runtime support is disabled")
	}

	pub(crate) fn reset(&mut self, _: Instant) {
		panic!("moq-clock runtime support is disabled")
	}

	pub(crate) fn poll(&mut self, _: &mut Context<'_>) -> Poll<()> {
		panic!("moq-clock runtime support is disabled")
	}
}

#[cfg(feature = "runtime")]
mod paused {
	use std::{
		collections::HashMap,
		sync::{
			LazyLock, Mutex,
			atomic::{AtomicBool, Ordering},
		},
	};

	use super::host;
	use crate::{Instant, Wall};

	/// A paused runtime counting from its own origin.
	struct Own {
		/// The first reading it counts from.
		origin: host::Instant,
		/// Its own [`anchor`], since its instants share no scale with [`ANCHOR`](super::ANCHOR).
		anchor: Option<(Instant, Wall)>,
	}

	/// Paused runtimes whose clock started before [`ORIGIN`](super::ORIGIN). Only test
	/// runtimes land here: an unpaused clock never reads below the origin. Never
	/// pruned; one entry per such runtime.
	static OWN: LazyLock<Mutex<HashMap<tokio::runtime::Id, Own>>> = LazyLock::new(Default::default);
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
			return own.origin;
		}
		if reading >= origin {
			return origin;
		}
		own.insert(
			runtime.id(),
			Own {
				origin: reading,
				anchor: None,
			},
		);
		ANY.store(true, Ordering::Release);
		reading
	}

	/// The anchor of the calling runtime, if it counts from its own origin.
	pub(super) fn anchor() -> Option<(Instant, Wall)> {
		// Read first: it registers a runtime whose clock starts before the origin.
		let now = super::now();
		if !ANY.load(Ordering::Acquire) {
			return None;
		}
		let runtime = tokio::runtime::Handle::try_current().ok()?;
		let mut own = OWN.lock().unwrap();
		let own = own.get_mut(&runtime.id())?;
		Some(*own.anchor.get_or_insert_with(|| (now, super::wall())))
	}
}

#[cfg(all(test, feature = "runtime"))]
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

	/// A paused runtime counting from its own origin anchors on its own clock, not
	/// on one another runtime advanced.
	#[test]
	fn paused_runtimes_own_their_anchors() {
		let paused = || {
			tokio::runtime::Builder::new_current_thread()
				.enable_time()
				.start_paused(true)
				.build()
				.unwrap()
		};
		// Built before anything reads the clock, so in a fresh process each counts
		// from its own origin.
		let first = paused();
		let second = paused();
		let _ = crate::now();
		first.block_on(async {
			let _ = crate::now();
			tokio::time::advance(Duration::from_secs(60)).await;
			let _ = crate::origin();
		});
		second.block_on(async {
			let (instant, _) = crate::origin();
			assert_eq!(instant, crate::now());
		});
	}

	/// A paused runtime sharing the origin cannot skew the anchor of unpaused
	/// callers by advancing its clock before the anchor is first taken.
	#[test]
	fn a_later_paused_runtime_does_not_skew_the_anchor() {
		let _ = crate::now();
		// Built after the origin, so it counts from the shared origin.
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.start_paused(true)
			.build()
			.unwrap();
		runtime.block_on(async {
			let _ = crate::now();
			tokio::time::advance(Duration::from_secs(3_600)).await;
			let _ = crate::origin();
		});
		let (instant, _) = crate::origin();
		assert!(instant <= crate::now(), "{instant:?} > {:?}", crate::now());
	}

	/// On a running clock, a converted instant is the same moment on std's clock.
	#[test]
	#[expect(clippy::disallowed_methods, reason = "the test compares against std's clock")]
	fn std_conversion_is_exact_on_a_running_clock() {
		let before = std::time::Instant::now();
		let now = crate::now();
		let after = std::time::Instant::now();
		let host = now.into_std();
		assert!(before <= host && host <= after, "{before:?} <= {host:?} <= {after:?}");
		assert_eq!(crate::Instant::from_std(host), now);
		let earlier = now - Duration::from_secs(3_600);
		assert_eq!(crate::Instant::from_std(earlier.into_std()), earlier);
	}

	/// On paused tokio, converted instants keep the time between them.
	#[test]
	fn std_conversion_keeps_paused_durations() {
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.start_paused(true)
			.build()
			.unwrap();
		runtime.block_on(async {
			let start = crate::now();
			let host = start.into_std();
			tokio::time::advance(Duration::from_secs(60)).await;
			let later = crate::now();
			assert_eq!(later.into_std() - host, Duration::from_secs(60));
			assert_eq!(
				crate::Instant::from_std(host + Duration::from_secs(5)),
				start + Duration::from_secs(5)
			);
		});
	}

	/// A std instant past the clock's range saturates instead of wrapping.
	#[test]
	fn std_conversion_saturates() {
		let now = crate::now();
		let far = now.into_std() + Duration::from_secs(1 << 40);
		assert!(crate::Instant::from_std(far) > now + Duration::from_secs(1 << 33));
	}

	/// A fresh clock names moments before the process started, as std's does, so
	/// looking back from startup never underflows.
	#[test]
	fn startup_reads_can_look_back() {
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.start_paused(true)
			.build()
			.unwrap();
		runtime.block_on(async {
			let now = crate::now();
			assert!(now.checked_sub(Duration::from_secs(3_600)).is_some(), "{now:?}");
		});
		assert!(crate::now().checked_sub(Duration::from_secs(3_600)).is_some());
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
