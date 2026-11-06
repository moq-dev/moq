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

use crate::clock::{Instant, Wall};

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

/// The host clock every unpaused runtime and plain thread shares, whichever runtime reads it.
#[cfg(feature = "runtime")]
pub(crate) fn host_now() -> Instant {
	real_now()
}

/// Without the runtime there are no paused runtimes, so the clock is the host's.
#[cfg(not(feature = "runtime"))]
pub(crate) fn host_now() -> Instant {
	now()
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
	/// When it was last armed, so firing can tell whether it waited.
	armed: host::Instant,
	/// Where tokio's clock read against std's when armed, while the guard is on.
	offset: Option<guard::Offset>,
}

#[cfg(feature = "runtime")]
impl Timer {
	pub(crate) fn new(at: Instant) -> Self {
		let (origin, armed) = read();
		Self {
			origin,
			sleep: Box::pin(host::sleep_until(origin + at.since_origin().saturating_sub(LEAD))),
			armed,
			offset: guard::offset(),
		}
	}

	pub(crate) fn reset(&mut self, at: Instant) {
		self.armed = host::Instant::now();
		self.offset = guard::offset();
		self.sleep
			.as_mut()
			.reset(self.origin + at.since_origin().saturating_sub(LEAD));
	}

	pub(crate) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
		// Checked first, so a reached deadline fires at once rather than at the
		// host timer's next tick.
		if host::Instant::now() < self.sleep.deadline() {
			std::task::ready!(self.sleep.as_mut().poll(cx));
		}
		if let Some(waited) = self.sleep.deadline().checked_duration_since(self.armed)
			&& !waited.is_zero()
		{
			guard::fired(waited, self.offset);
		}
		Poll::Ready(())
	}
}

/// Let timers wait on the host clock in this process, as real I/O does: real sockets, devices, or processes.
///
/// With `KIO_DENY_HOST_TIMERS=1` in the environment, a timer that waits on the host clock
/// aborts the process unless it called this. It lasts for the process, which is one test
/// under nextest.
pub fn allow_host_timers() {
	#[cfg(feature = "runtime")]
	guard::ALLOWED.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Fails a test whose timer waits on the host clock, switched on by the environment.
///
/// Only the host backend waits in real time; paused tokio, the simulator, a
/// [`Manual`](crate::clock::Manual) clock, and a [`Deadlines`](crate::clock::Deadlines)
/// queue all move only when told. A watchdog that its future beats never fires, so
/// only waits that ran out count: a negative proof, a settle sleep, a real interval.
#[cfg(feature = "runtime")]
mod guard {
	use std::{
		io::Write,
		sync::{
			OnceLock,
			atomic::{AtomicBool, Ordering},
		},
		time::Duration,
	};

	use super::host;

	/// The variable that denies host timers, which the repository's test runner sets.
	const VAR: &str = "KIO_DENY_HOST_TIMERS";

	pub(super) static ALLOWED: AtomicBool = AtomicBool::new(false);

	fn denied() -> bool {
		static DENIED: OnceLock<bool> = OnceLock::new();
		*DENIED.get_or_init(|| match std::env::var_os(VAR) {
			None => false,
			Some(value) if value == "1" => true,
			Some(value) => panic!("{VAR} must be 1 or unset, not {value:?}"),
		})
	}

	/// Bounds on how far tokio's clock reads from std's, in nanoseconds.
	///
	/// A running tokio clock, resumed after a pause included, keeps a fixed offset from
	/// std's, so the bounds taken when a timer is armed and when it fires overlap. Pausing
	/// or advancing it moves the offset by the time it skipped.
	#[derive(Clone, Copy)]
	pub(super) struct Offset {
		low: i128,
		high: i128,
	}

	/// The current [`Offset`], while the guard is on.
	pub(super) fn offset() -> Option<Offset> {
		if !denied() {
			return None;
		}
		let before = std::time::Instant::now();
		let reading = host::Instant::now().into_std();
		let after = std::time::Instant::now();
		let signed = |from: std::time::Instant| match reading.checked_duration_since(from) {
			Some(ahead) => ahead.as_nanos() as i128,
			None => -((from - reading).as_nanos() as i128),
		};
		Some(Offset {
			low: signed(after),
			high: signed(before),
		})
	}

	/// A host timer armed at `armed`'s offset fired after waiting `waited`.
	pub(super) fn fired(waited: Duration, armed: Option<Offset>) {
		let (Some(armed), Some(now)) = (armed, offset()) else {
			return;
		};
		// A paused or advanced clock moved its offset; one that ran kept it.
		if ALLOWED.load(Ordering::Relaxed) || now.high < armed.low || now.low > armed.high {
			return;
		}
		// Abort rather than panic: a panic in a spawned task never reaches the test.
		// Written to the handle, since libtest's capture of `eprintln!` dies with the process.
		let _ = writeln!(
			std::io::stderr(),
			"kio: a {waited:?} timer fired on the host clock, which {VAR}=1 denies.\n\
			 Run the test on a mocked clock (`#[tokio::test(start_paused = true)]`, `#[kio::test]`, \
			 or `kio::clock::Manual`), or await the event it waits for. A test on real sockets, \
			 devices, or processes calls `kio::clock::allow_host_timers()`, and clears {VAR} from \
			 any process it spawns.\n{}",
			std::backtrace::Backtrace::force_capture()
		);
		std::process::abort();
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
		panic!("the kio `runtime` feature is disabled")
	}

	pub(crate) fn reset(&mut self, _: Instant) {
		panic!("the kio `runtime` feature is disabled")
	}

	pub(crate) fn poll(&mut self, _: &mut Context<'_>) -> Poll<()> {
		panic!("the kio `runtime` feature is disabled")
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
	use crate::clock::{Instant, Wall};

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
		let _ = crate::clock::now();
		runtime.block_on(async {
			let start = crate::clock::now();
			crate::clock::sleep(Duration::from_secs(5)).await;
			assert_eq!(crate::clock::now(), start + Duration::from_secs(5));
			tokio::time::advance(Duration::from_secs(60)).await;
			assert_eq!(crate::clock::now(), start + Duration::from_secs(65));
			let at = crate::clock::sleep_until(start + Duration::from_secs(70));
			assert_eq!(at.deadline(), start + Duration::from_secs(70));
			at.await;
			assert_eq!(crate::clock::now(), start + Duration::from_secs(70));
		});
	}

	/// A paused runtime counting from its own origin anchors on its own clock, not
	/// on one another runtime advanced.
	#[test]
	#[cfg_attr(miri, ignore = "re-runs itself in a fresh process, which Miri cannot spawn")]
	fn paused_runtimes_own_their_anchors() {
		// A fresh process keeps other tests from taking the shared origin first.
		if !test_env::Env::default().set("MOQ_CLOCK_TEST_ORIGIN", "fresh").isolate() {
			return;
		}
		assert!(super::ORIGIN.get().is_none());
		let paused = || {
			tokio::runtime::Builder::new_current_thread()
				.enable_time()
				.start_paused(true)
				.build()
				.unwrap()
		};
		// Built before the shared origin, so each counts from its own origin.
		let first = paused();
		let second = paused();
		let _ = crate::clock::now();
		first.block_on(async {
			assert_eq!(crate::clock::now(), crate::clock::Instant::from_origin(super::LEAD));
			tokio::time::advance(Duration::from_secs(60)).await;
			let _ = crate::clock::origin();
		});
		second.block_on(async {
			assert_eq!(crate::clock::now(), crate::clock::Instant::from_origin(super::LEAD));
			let (instant, _) = crate::clock::origin();
			assert_eq!(instant, crate::clock::now());
		});
	}

	/// A paused runtime sharing the origin cannot skew the anchor of unpaused
	/// callers by advancing its clock before the anchor is first taken.
	#[test]
	#[cfg_attr(miri, ignore = "reads the wall clock, which Miri's isolation refuses")]
	fn a_later_paused_runtime_does_not_skew_the_anchor() {
		let _ = crate::clock::now();
		// Built after the origin, so it counts from the shared origin.
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.start_paused(true)
			.build()
			.unwrap();
		runtime.block_on(async {
			let _ = crate::clock::now();
			tokio::time::advance(Duration::from_secs(3_600)).await;
			let _ = crate::clock::origin();
		});
		let (instant, _) = crate::clock::origin();
		assert!(
			instant <= crate::clock::now(),
			"{instant:?} > {:?}",
			crate::clock::now()
		);
	}

	/// On a running clock, a converted instant is the same moment on std's clock.
	#[test]
	#[expect(clippy::disallowed_methods, reason = "the test compares against std's clock")]
	fn std_conversion_is_exact_on_a_running_clock() {
		let before = std::time::Instant::now();
		let now = crate::clock::now();
		let after = std::time::Instant::now();
		let host = now.into_std();
		assert!(before <= host && host <= after, "{before:?} <= {host:?} <= {after:?}");
		assert_eq!(crate::clock::Instant::from_std(host), now);
		let earlier = now - Duration::from_secs(3_600);
		assert_eq!(crate::clock::Instant::from_std(earlier.into_std()), earlier);
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
			let start = crate::clock::now();
			let host = start.into_std();
			tokio::time::advance(Duration::from_secs(60)).await;
			let later = crate::clock::now();
			assert_eq!(later.into_std() - host, Duration::from_secs(60));
			assert_eq!(
				crate::clock::Instant::from_std(host + Duration::from_secs(5)),
				start + Duration::from_secs(5)
			);
		});
	}

	/// A std instant past the clock's range saturates instead of wrapping.
	#[test]
	fn std_conversion_saturates() {
		let now = crate::clock::now();
		let far = now.into_std() + Duration::from_secs(1 << 40);
		assert!(crate::clock::Instant::from_std(far) > now + Duration::from_secs(1 << 33));
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
			let now = crate::clock::now();
			assert!(now.checked_sub(Duration::from_secs(3_600)).is_some(), "{now:?}");
		});
		assert!(crate::clock::now().checked_sub(Duration::from_secs(3_600)).is_some());
	}

	/// Run the ignored `guard::` case `name` in a child process that denies host timers,
	/// returning whether the guard aborted it.
	fn aborts(name: &str) -> bool {
		let test = format!("clock::runtime::tests::guard::{name}");
		let output = std::process::Command::new(std::env::current_exe().unwrap())
			.args([&test, "--exact", "--ignored", "--test-threads=1"])
			.env("KIO_DENY_HOST_TIMERS", "1")
			.output()
			.unwrap();
		let stdout = String::from_utf8_lossy(&output.stdout);
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert!(stdout.contains("running 1 test"), "no case named {name}: {stdout}");
		let aborted = stderr.contains("timer fired on the host clock");
		assert_eq!(output.status.success(), !aborted, "{stdout}{stderr}");
		aborted
	}

	#[test]
	#[cfg_attr(miri, ignore = "spawns a child process, which Miri cannot")]
	fn a_wait_on_the_host_clock_aborts() {
		assert!(aborts("sleep"));
	}

	#[test]
	#[cfg_attr(miri, ignore = "spawns a child process, which Miri cannot")]
	fn a_real_interval_aborts_on_its_second_tick() {
		assert!(aborts("interval"));
	}

	#[test]
	#[cfg_attr(miri, ignore = "spawns a child process, which Miri cannot")]
	fn a_resumed_runtime_aborts() {
		assert!(aborts("resumed"));
	}

	#[test]
	#[cfg_attr(miri, ignore = "spawns a child process, which Miri cannot")]
	fn a_wait_in_a_spawned_task_aborts() {
		assert!(aborts("spawned"));
	}

	#[test]
	#[cfg_attr(miri, ignore = "spawns a child process, which Miri cannot")]
	fn waits_that_never_run_out_pass() {
		assert!(!aborts("unexpired"));
	}

	#[test]
	#[cfg_attr(miri, ignore = "spawns a child process, which Miri cannot")]
	fn a_paused_runtime_passes() {
		assert!(!aborts("paused"));
	}

	#[test]
	#[cfg_attr(miri, ignore = "spawns a child process, which Miri cannot")]
	fn an_allowed_wait_passes() {
		assert!(!aborts("allowed"));
	}

	/// The cases [`aborts`] runs, each in a process of its own.
	mod guard {
		use std::time::Duration;

		fn real() -> tokio::runtime::Runtime {
			tokio::runtime::Builder::new_current_thread()
				.enable_time()
				.build()
				.unwrap()
		}

		#[test]
		#[ignore = "aborts; a_wait_on_the_host_clock_aborts runs it in a child"]
		fn sleep() {
			real().block_on(async { crate::clock::sleep(Duration::from_millis(1)).await });
		}

		#[test]
		#[ignore = "aborts; a_real_interval_aborts_on_its_second_tick runs it in a child"]
		fn interval() {
			real().block_on(async {
				let mut interval = crate::clock::interval(Duration::from_millis(1));
				// So the second tick waits even if the first was late.
				interval.set_missed_tick_behavior(crate::clock::MissedTickBehavior::Delay);
				interval.tick().await;
				interval.tick().await;
			});
		}

		#[test]
		#[ignore = "aborts; a_resumed_runtime_aborts runs it in a child"]
		fn resumed() {
			tokio::runtime::Builder::new_current_thread()
				.enable_time()
				.start_paused(true)
				.build()
				.unwrap()
				.block_on(async {
					tokio::time::advance(Duration::from_secs(3_600)).await;
					tokio::time::resume();
					crate::clock::sleep(Duration::from_millis(1)).await;
				});
		}

		#[test]
		#[ignore = "aborts; a_wait_in_a_spawned_task_aborts runs it in a child"]
		fn spawned() {
			let runtime = real();
			let task = runtime.spawn(async { crate::clock::sleep(Duration::from_millis(1)).await });
			// The task's panic would reach the test only through its handle.
			let _ = runtime.block_on(task);
		}

		/// A first interval tick, a zero sleep, and a watchdog its future beats.
		#[test]
		#[ignore = "waits_that_never_run_out_pass runs it in a child"]
		fn unexpired() {
			real().block_on(async {
				crate::clock::interval(Duration::from_secs(1)).tick().await;
				crate::clock::sleep(Duration::ZERO).await;
				let mut sleep = std::pin::pin!(crate::clock::sleep(Duration::from_secs(3_600)));
				sleep.as_mut().reset(crate::clock::now());
				sleep.await;
				crate::clock::timeout(Duration::from_secs(3_600), tokio::task::yield_now())
					.await
					.unwrap();
			});
		}

		#[test]
		#[ignore = "a_paused_runtime_passes runs it in a child"]
		fn paused() {
			tokio::runtime::Builder::new_current_thread()
				.enable_time()
				.start_paused(true)
				.build()
				.unwrap()
				.block_on(async { crate::clock::sleep(Duration::from_secs(3_600)).await });
		}

		#[test]
		#[ignore = "an_allowed_wait_passes runs it in a child"]
		fn allowed() {
			crate::clock::allow_host_timers();
			real().block_on(async { crate::clock::sleep(Duration::from_millis(1)).await });
		}
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
		let before = crate::clock::now();
		let reads = runtime.block_on(async {
			let tasks: Vec<_> = (0..16).map(|_| tokio::spawn(async { crate::clock::now() })).collect();
			let mut reads = Vec::new();
			for task in tasks {
				reads.push(task.await.unwrap());
			}
			reads
		});
		let after = crate::clock::now();
		for read in reads {
			assert!(before <= read && read <= after, "{before:?} <= {read:?} <= {after:?}");
		}
	}
}
