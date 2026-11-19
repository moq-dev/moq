//! The ambient clock, captured so it reads the same from anywhere.

use std::time::Duration;

use crate::clock::{Instant, Wall};

/// The ambient clock as it was when [`current`](Self::current) captured it.
///
/// It keeps reading that clock from any thread, runtime, or clock context, so stamps taken
/// through it share one timeline wherever they are taken. The ambient [`now`](crate::clock::now)
/// reads whichever clock the caller happens to be in, and instants from different clocks don't
/// compare. Clones read the same clock.
#[derive(Clone)]
pub struct Clock(Source);

#[derive(Clone)]
enum Source {
	/// The host clock every unpaused runtime and plain thread shares.
	Host,
	/// A tokio runtime's own clock, which differs from the host's while it is paused.
	#[cfg(feature = "runtime")]
	Runtime(tokio::runtime::Handle),
	/// A manual clock installed on a tokio runtime.
	#[cfg(feature = "manual")]
	Installed(crate::clock::Installed),
	/// A simulator run, by id: only a thread inside that run can read it.
	#[cfg(feature = "sim")]
	Sim(u64),
}

impl Clock {
	/// Capture the ambient clock, in the order [`now`](crate::clock::now) picks it.
	///
	/// An entered [`Deadlines`](crate::clock::Deadlines) queue is a view of the clock it is
	/// advanced from, so the capture is that clock: the installed or simulated one, else the host's.
	pub fn current() -> Self {
		let entered = super::deadline::entered().is_some();
		#[cfg(feature = "manual")]
		if entered && let Some(clock) = super::deadline::installed() {
			return Self(Source::Installed(clock));
		}
		#[cfg(feature = "sim")]
		if let Some(id) = super::sim::run_id() {
			return Self(Source::Sim(id));
		}
		if entered {
			return Self(Source::Host);
		}
		#[cfg(feature = "manual")]
		if let Some(clock) = super::installed::current() {
			return Self(Source::Installed(clock));
		}
		#[cfg(feature = "runtime")]
		if let Ok(runtime) = tokio::runtime::Handle::try_current() {
			return Self(Source::Runtime(runtime));
		}
		Self(Source::Host)
	}

	/// The current instant on this clock.
	///
	/// # Panics
	///
	/// When captured inside a simulator run and read outside it.
	pub fn now(&self) -> Instant {
		match &self.0 {
			Source::Host => super::runtime::host_now(),
			#[cfg(feature = "runtime")]
			Source::Runtime(runtime) => {
				let _entered = runtime.enter();
				super::runtime::now()
			}
			#[cfg(feature = "manual")]
			Source::Installed(clock) => clock.now(),
			#[cfg(feature = "sim")]
			Source::Sim(id) => super::sim::now_in(*id),
		}
	}

	/// The current wall time on this clock.
	///
	/// # Panics
	///
	/// When captured inside a simulator run and read outside it, or when the host's wall clock
	/// reads before 1970.
	pub fn wall(&self) -> Wall {
		match &self.0 {
			Source::Host => super::runtime::wall(),
			#[cfg(feature = "runtime")]
			Source::Runtime(runtime) => {
				let _entered = runtime.enter();
				super::runtime::runtime_wall()
			}
			#[cfg(feature = "manual")]
			Source::Installed(clock) => clock.wall(),
			#[cfg(feature = "sim")]
			Source::Sim(id) => super::sim::wall_in(*id),
		}
	}

	/// How long the host has spent suspended, which [`now`](Self::now) does not count.
	///
	/// Its growth between two reads is how long the host slept between them, so adding it to the
	/// instants' difference measures time as the wall clock does across a suspend. The host's reads
	/// the platform where its monotonic clock stops while asleep (Linux, Android, Apple) and is zero
	/// elsewhere, as on Windows, whose monotonic clock already counts sleep. A paused runtime reads
	/// the host's too. Two reads taken within nanoseconds of each other can differ by that much.
	///
	/// # Panics
	///
	/// When captured inside a simulator run and read outside it.
	pub fn suspended(&self) -> Duration {
		match &self.0 {
			Source::Host => super::runtime::suspended(),
			#[cfg(feature = "runtime")]
			Source::Runtime(_) => super::runtime::suspended(),
			#[cfg(feature = "manual")]
			Source::Installed(clock) => clock.suspended(),
			#[cfg(feature = "sim")]
			Source::Sim(id) => super::sim::suspended_in(*id),
		}
	}
}

/// Two captures are equal when they read the same clock.
impl PartialEq for Clock {
	fn eq(&self, other: &Self) -> bool {
		match (&self.0, &other.0) {
			(Source::Host, Source::Host) => true,
			#[cfg(feature = "runtime")]
			(Source::Runtime(a), Source::Runtime(b)) => a.id() == b.id(),
			#[cfg(feature = "manual")]
			(Source::Installed(a), Source::Installed(b)) => a.same(b),
			#[cfg(feature = "sim")]
			(Source::Sim(a), Source::Sim(b)) => a == b,
			#[allow(unreachable_patterns, reason = "only the host clock exists without features")]
			_ => false,
		}
	}
}

impl Eq for Clock {}

impl std::fmt::Debug for Clock {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match &self.0 {
			Source::Host => f.write_str("Host"),
			#[cfg(feature = "runtime")]
			Source::Runtime(runtime) => f.debug_tuple("Runtime").field(&runtime.id()).finish(),
			#[cfg(feature = "manual")]
			Source::Installed(clock) => std::fmt::Debug::fmt(clock, f),
			#[cfg(feature = "sim")]
			Source::Sim(id) => f.debug_tuple("Sim").field(id).finish(),
		}
	}
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::Clock;

	fn paused() -> tokio::runtime::Runtime {
		tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.start_paused(true)
			.build()
			.unwrap()
	}

	/// A clock captured in a paused runtime keeps reading it from another runtime and a plain
	/// thread, where the ambient clock is a different one.
	#[test]
	fn a_paused_runtime_reads_the_same_everywhere() {
		let first = paused();
		let (clock, at) = first.block_on(async {
			tokio::time::advance(Duration::from_secs(60)).await;
			let clock = Clock::current();
			(clock.clone(), clock.now())
		});
		assert_eq!(clock.now(), at, "the paused clock did not move");

		let second = paused();
		second.block_on(async {
			tokio::time::advance(Duration::from_secs(3600)).await;
			assert_eq!(clock.now(), at, "another runtime's time is not this clock's");
			assert_ne!(Clock::current(), clock);
		});
		std::thread::scope(|scope| {
			scope.spawn(|| assert_eq!(clock.now(), at));
		});

		first.block_on(async {
			tokio::time::advance(Duration::from_secs(5)).await;
			assert_eq!(Clock::current(), clock);
		});
		assert_eq!(clock.now(), at.saturating_add(Duration::from_secs(5)));
	}

	/// An installed manual clock is read wherever its capture goes.
	#[test]
	fn an_installed_clock_reads_the_same_everywhere() {
		let runtime = tokio::runtime::Builder::new_current_thread()
			.enable_time()
			.build()
			.unwrap();
		let manual = crate::clock::Manual::new().install(runtime.handle());
		let clock = runtime.block_on(async { Clock::current() });
		manual.advance(Duration::from_secs(7));
		assert_eq!(clock.now(), manual.now());
		assert_eq!(clock.wall(), manual.wall());
		paused().block_on(async { assert_eq!(clock.now(), manual.now()) });
	}

	/// A capture inside the simulator reads simulated time, and one on a plain thread reads the
	/// host's even from inside the simulator.
	#[test]
	fn the_simulator_and_the_host_stay_apart() {
		let host = Clock::current();
		crate::clock::sim::run(async move {
			let sim = Clock::current();
			crate::clock::sleep(Duration::from_secs(10)).await;
			assert_eq!(
				sim.now(),
				crate::clock::Instant::ZERO.saturating_add(Duration::from_secs(10))
			);
			assert_eq!(sim.wall(), crate::clock::Manual::WALL_START + Duration::from_secs(10));
			assert!(host.now() > crate::clock::Instant::ZERO.saturating_add(Duration::from_secs(1 << 31)));
			assert_ne!(sim, host);
		});
	}

	/// A simulated suspend counts as suspended without moving the instant, and the host's reads.
	#[test]
	fn a_suspend_is_counted_apart_from_the_instant() {
		// The host's clocks read, and its sleep never shrinks beyond the gap between two reads.
		let host = Clock::current();
		let slept = host.suspended();
		assert!(host.suspended() + Duration::from_micros(100) >= slept);

		crate::clock::sim::run(async {
			let clock = Clock::current();
			let start = clock.now();
			crate::clock::sim::suspend(Duration::from_secs(3600));
			assert_eq!(clock.now(), start);
			assert_eq!(clock.suspended(), Duration::from_secs(3600));
			assert_eq!(
				clock.wall(),
				crate::clock::Manual::WALL_START + Duration::from_secs(3600)
			);
		});
	}

	/// A queue entered on a thread is read through the clock it is advanced from.
	#[test]
	fn an_entered_queue_captures_its_clock() {
		crate::clock::sim::run(async {
			let deadlines = crate::clock::Deadlines::new(crate::clock::now());
			crate::clock::sim::attach(&deadlines);
			let clock = {
				let _entered = deadlines.enter();
				Clock::current()
			};
			assert_eq!(clock, Clock::current());
		});
	}
}
