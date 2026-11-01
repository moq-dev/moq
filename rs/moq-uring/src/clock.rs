//! The worker's clock: the only module that reads the host clock.
//!
//! The worker samples it once per turn and hands that instant to its
//! [`kio::clock::Deadlines`] queue, which it enters as the thread's ambient clock,
//! so everything it runs reads the sample instead of the host clock. The host
//! clock is `CLOCK_MONOTONIC`, read directly so the park can hand the next
//! deadline to the kernel as an absolute timeout without a second read.

use std::task::Context;
use std::time::Duration;

use kio::clock::Instant;

/// Where a worker's time comes from.
#[derive(Clone)]
pub(crate) enum Clock {
	Host(Host),
	/// A manual clock installed on the tokio runtime the worker was created in,
	/// so time passes only when the test advances it.
	///
	/// The host clock alongside bounds teardown, which must end however long
	/// the test leaves its clock alone.
	#[cfg(feature = "_manual")]
	Installed(kio::clock::Installed, Host),
}

/// How the worker waits for its next deadline while parked.
pub(crate) enum Wait {
	/// The deadline already passed: run another turn instead of parking.
	#[cfg_attr(
		not(feature = "_manual"),
		allow(dead_code, reason = "only the `_manual` feature uses it")
	)]
	Elapsed,
	/// Park until a completion or a wake, with no timeout.
	Forever,
	/// Park until a completion, a wake, or this `CLOCK_MONOTONIC` reading.
	Until(Duration),
}

impl Clock {
	/// The clock installed on the current tokio runtime, or else the host's.
	pub fn new() -> Self {
		#[cfg(feature = "_manual")]
		if let Some(clock) = kio::clock::Installed::current() {
			return Self::Installed(clock, Host::calibrate());
		}
		Self::Host(Host::calibrate())
	}

	/// Read the clock.
	pub fn now(&self) -> Instant {
		match self {
			Self::Host(host) => host.now(),
			#[cfg(feature = "_manual")]
			Self::Installed(clock, _) => clock.now(),
		}
	}

	/// The host's time, which passes on any clock: for a safety bound that must
	/// expire even while a manual clock stands still.
	pub fn host_now(&self) -> Instant {
		match self {
			Self::Host(host) => host.now(),
			#[cfg(feature = "_manual")]
			Self::Installed(_, host) => host.now(),
		}
	}

	/// How to park for `deadline`. A manual clock keeps its alarm in `alarm`, so
	/// advancing it wakes the worker through `cx`.
	pub fn wait(&self, deadline: Option<Instant>, alarm: &mut Option<kio::clock::Sleep>, cx: &mut Context<'_>) -> Wait {
		match self {
			Self::Host(host) => {
				let _ = (alarm, cx);
				deadline.map_or(Wait::Forever, |at| Wait::Until(host.monotonic(at)))
			}
			#[cfg(feature = "_manual")]
			Self::Installed(clock, _) => {
				use std::future::Future;

				let Some(at) = deadline else {
					*alarm = None;
					return Wait::Forever;
				};
				let sleep = alarm.get_or_insert_with(|| clock.sleep_until(at));
				if sleep.deadline() != at {
					sleep.reset(at);
				}
				match std::pin::Pin::new(sleep).poll(cx) {
					std::task::Poll::Ready(()) => Wait::Elapsed,
					std::task::Poll::Pending => Wait::Forever,
				}
			}
		}
	}
}

/// `CLOCK_MONOTONIC`, mapped onto `kio::clock`'s instants at one calibration point.
///
/// `kio::clock`'s host clock reads the same monotonic clock from an origin it keeps
/// to itself, so the two differ by a constant that one reading of each pins down
/// to within the gap between them.
#[derive(Clone, Copy)]
pub(crate) struct Host {
	/// A monotonic reading.
	monotonic: Duration,
	/// `kio::clock`'s instant at that reading.
	instant: Instant,
}

impl Host {
	fn calibrate() -> Self {
		let before = monotonic();
		let instant = kio::clock::now();
		let after = monotonic();
		Self {
			monotonic: before + (after - before) / 2,
			instant,
		}
	}

	fn now(&self) -> Instant {
		self.instant + monotonic().saturating_sub(self.monotonic)
	}

	/// The monotonic reading at `at`; one before calibration maps to calibration.
	fn monotonic(&self, at: Instant) -> Duration {
		self.monotonic + at.saturating_duration_since(self.instant)
	}
}

/// The host's monotonic clock: one vDSO call.
fn monotonic() -> Duration {
	let mut now = libc::timespec { tv_sec: 0, tv_nsec: 0 };
	// SAFETY: valid out-pointer.
	unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
	Duration::new(now.tv_sec as u64, now.tv_nsec as u32)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The calibrated clock agrees with `kio::clock`'s host clock, which reads the same
	/// monotonic clock from another origin.
	#[test]
	fn host_tracks_kio_clock() {
		// Off by at most the gap between the two calibration reads, which a
		// preemption can stretch, so measure it rather than assume one.
		let start = monotonic();
		let host = Host::calibrate();
		let slack = monotonic() - start;
		let before = kio::clock::now();
		let now = host.now();
		let after = kio::clock::now();
		assert!(
			before <= now + slack && now <= after + slack,
			"{before:?} <= {now:?} <= {after:?}"
		);
	}

	/// The host's time keeps passing while a manual clock stands still, so the
	/// teardown bound it measures expires.
	#[cfg(feature = "_manual")]
	#[test]
	fn host_time_passes_on_a_standing_manual_clock() {
		let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
		let manual = kio::clock::Manual::new().install(runtime.handle());
		let _entered = runtime.enter();
		let clock = Clock::new();
		let start = clock.host_now();
		while clock.host_now() == start {
			std::hint::spin_loop();
		}
		assert_eq!(clock.now(), manual.now(), "the manual clock moved");
	}

	/// A deadline lands the same distance from the calibration point on both clocks.
	#[test]
	fn deadlines_map_onto_the_monotonic_clock() {
		let host = Host {
			monotonic: Duration::from_secs(100),
			instant: Instant::ZERO + Duration::from_secs(1 << 32),
		};
		assert_eq!(
			host.monotonic(host.instant + Duration::from_millis(1500)),
			Duration::from_millis(101_500)
		);
		assert_eq!(
			host.monotonic(Instant::ZERO),
			Duration::from_secs(100),
			"one before calibration is already due"
		);
	}
}
