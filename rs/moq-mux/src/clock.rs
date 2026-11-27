//! The broadcast's shared clock: one monotonic timeline plus its wall mapping.
//!
//! Create one [`Clock`] per broadcast and hand clones to every producer: because they share a
//! timeline, frames captured at the same instant get the same timestamp, keeping concurrently
//! produced tracks (e.g. audio and video capture on separate threads) in sync. Cloning is cheap.
//! Taking the catalog's clock fixes it, so a clone never goes stale.
//!
//! A clock reads the ambient clock it was created under (see [`kio::clock::Clock`]), wherever it
//! is read from later. A broadcast built under one runtime and written from another, or from a
//! plain thread, stays on one timeline instead of jumping between the two clocks.
//!
//! The timeline runs at the host wall clock's rate rather than its monotonic clock's, so `wall + pts`
//! stays on UTC for any session length. A monotonic clock drifts off UTC by its oscillator's error
//! (seconds a day on macOS and Windows), so each read nudges the timeline toward the wall clock by at
//! most 500 ppm, NTP's own slew cap: a wall clock that steps either way is followed that slowly,
//! and media never jumps. A suspend is a gap instead, which the timeline jumps forward across, since
//! no media exists for that time: it counts the host's [`suspended`](kio::clock::Clock::suspended)
//! time, where the monotonic clock stops.
//!
//! The clock also owns the broadcast's wall mapping, advertised at the catalog root as
//! `clock: { wall, timescale }`: `wall` is the wall-clock time of PTS zero in
//! [`Clock::TIMESCALE`] units since the moq epoch (2020-01-01). A consumer derives any
//! timestamp's wall time as `wall + pts` after converting it into this timescale. A discontinuity
//! marker is a delivery event, not a new epoch, and a system-clock adjustment never retimes it.
//! A host whose wall clock reads before 2020 has not set it yet (a device booted before NTP sync),
//! so its clock carries no mapping and the catalog omits `clock` rather than advertise a false one.
//!
//! A container importer publishes its stream's own timestamps. The first to receive a frame before
//! anything fixes the clock places the mapping so that frame is live on arrival; any other shifts
//! onto the clock (see [`catalog::Timebase`](crate::catalog::Timebase)).

use kio::clock::Wall;
use std::{
	sync::{Arc, Mutex, PoisonError},
	time::Duration,
};

use hang::catalog::MAX_SAFE_INTEGER;
use kio::clock::Instant;

/// Microseconds from the moq epoch (2020) to `wall`, negative before it.
fn moq_micros(wall: Wall) -> i128 {
	let since = |from: Wall, to: Wall| to.saturating_duration_since(from).as_micros() as i128;
	since(Wall::MOQ_EPOCH, wall) - since(wall, Wall::MOQ_EPOCH)
}

/// The host's wall clock, or `None` while it reads before the moq epoch (2020): unset.
pub(crate) fn host_wall() -> Option<Wall> {
	set(kio::clock::wall())
}

/// `now`, or `None` while it reads before the moq epoch (2020): a wall clock not yet set.
fn set(now: Wall) -> Option<Wall> {
	#[cfg(test)]
	let now = HOST_WALL.get().unwrap_or(now);
	(moq_micros(now) >= 0).then_some(now)
}

#[cfg(test)]
thread_local! {
	/// What [`host_wall`] reads on this thread instead of the system clock.
	static HOST_WALL: std::cell::Cell<Option<Wall>> = const { std::cell::Cell::new(None) };
}

/// Make this thread's host wall clock read `now`, so a test can fake one that was never set.
#[cfg(test)]
pub(crate) fn fake_host_wall(now: Wall) {
	HOST_WALL.set(Some(now));
}

/// The catalog clock for PTS zero at `wall`.
fn wall_clock(wall: Wall) -> crate::Result<hang::catalog::Clock> {
	// A time before 2020 cannot be named on the wire; refuse it rather than advertise 2020.
	let micros = moq_micros(wall);
	if micros < 0 {
		return Err(hang::Error::InvalidWall(0).into());
	}
	let wall = u64::try_from(micros).unwrap_or(u64::MAX);
	if wall > MAX_SAFE_INTEGER {
		return Err(hang::Error::InvalidWall(wall).into());
	}
	Ok(hang::catalog::Clock::new(moq_net::Timestamp::from_micros(wall)?)?)
}

/// A clock for stamping media frames so that tracks produced concurrently, e.g. an audio and a
/// video capture running on separate threads, land on a single timeline.
///
/// It never runs backward, and runs at the host wall clock's rate: each read nudges it toward the
/// wall clock by at most 500 ppm, and it jumps forward across a suspend.
/// Clones share the timeline and the wall mapping, so handing them to several producers keeps
/// the broadcast on one clock. Two clocks are equal only when they are one timeline.
#[derive(Clone, Debug)]
pub struct Clock {
	/// The ambient clock captured where this one was created, read wherever it is read.
	source: kio::clock::Clock,
	timeline: Arc<Mutex<Timeline>>,
	/// `None` when the broadcast has no wall mapping: the host's wall clock was unset at construction, or
	/// the source advertised none.
	wall: Option<hang::catalog::Clock>,
}

/// The most a timeline gains or loses on the host's monotonic time to follow its wall clock, as a
/// divisor: 500 ppm, the cap NTP slews the host's own clocks by.
const SLEW: u64 = 2_000;

/// A wall error, in nanoseconds, left alone, so two clocks read a moment apart don't dither the
/// timeline's rate.
const SETTLED: i128 = 1_000_000;

/// Nanoseconds from the Unix epoch to `wall`.
fn unix_nanos(wall: Wall) -> i128 {
	wall.saturating_duration_since(Wall::UNIX_EPOCH).as_nanos() as i128
}

/// Where a clock's timeline last read, and the wall clock it follows.
#[derive(Debug)]
struct Timeline {
	/// The host's monotonic instant and the time it had spent suspended at the last read.
	instant: Instant,
	suspended: Duration,
	/// What the clock read then, in nanoseconds.
	reading: u64,
	/// The host wall time, in Unix nanoseconds, at which the timeline reads zero; `None` when the
	/// host's wall clock was unset at the start, leaving nothing to follow.
	zero: Option<i128>,
}

impl Timeline {
	/// A timeline reading `reading` nanoseconds at `instant` on `source`, which may be past or ahead.
	fn start(source: &kio::clock::Clock, instant: Instant, reading: u64) -> Arc<Mutex<Self>> {
		let now = source.now();
		let ahead = now.saturating_duration_since(instant).as_nanos() as i128
			- instant.saturating_duration_since(now).as_nanos() as i128;
		Arc::new(Mutex::new(Self {
			instant,
			suspended: source.suspended(),
			reading,
			zero: set(source.wall()).map(|wall| unix_nanos(wall) - ahead - reading as i128),
		}))
	}

	/// What the timeline reads at `instant`, after `suspended` of sleep, with the host wall at `wall`.
	///
	/// It advances by the time that passed, suspends included, nudged toward the wall clock by at
	/// most [`SLEW`] of it, so it never runs backward and a wall step is followed gradually.
	fn read(&mut self, instant: Instant, suspended: Duration, wall: Option<Wall>) -> u64 {
		// Each kept at its largest, so a read's jitter is counted once rather than piling up.
		let elapsed = instant.saturating_duration_since(self.instant) + suspended.saturating_sub(self.suspended);
		self.instant = self.instant.max(instant);
		self.suspended = self.suspended.max(suspended);

		// u128 -> u64 truncation is unreachable: u64 nanoseconds is ~584 years.
		let elapsed = elapsed.as_nanos() as u64;
		let free = self.reading.saturating_add(elapsed);
		let error = match (self.zero, wall) {
			(Some(zero), Some(wall)) => unix_nanos(wall) - zero - free as i128,
			_ => 0,
		};
		let cap = (elapsed / SLEW) as i128;
		let nudge = if error.abs() <= SETTLED {
			0
		} else {
			error.clamp(-cap, cap)
		};
		// Within u64: the nudge is at most what elapsed, either way.
		self.reading = (free as i128 + nudge) as u64;
		self.reading
	}
}

impl Clock {
	/// Units per second for the broadcast clock: microseconds.
	///
	/// Matches the hang container timescale, so media timestamps convert without loss, and fine
	/// enough that the wall value stays within the JSON-safe integer range until the year 2305.
	pub const TIMESCALE: moq_net::Timescale = moq_net::Timescale::MICRO;

	/// What a fresh clock reads at construction.
	///
	/// Frames stamped beside it can carry slightly earlier timestamps (audio captured a moment
	/// before the video it is muxed with). Starting the clock this far past zero leaves them room
	/// instead of landing before the broadcast began.
	const LEAD: Duration = Duration::from_secs(10);

	/// Start a clock that reads ten seconds now, so PTS zero is ten seconds ago on the wall.
	///
	/// Without a wall mapping when the host's wall clock is unset, or reads a time the mapping can't
	/// name: media needs only the monotonic timeline.
	pub fn new() -> Self {
		let source = kio::clock::Clock::current();
		Self {
			timeline: Timeline::start(&source, source.now(), Self::LEAD.as_nanos() as u64),
			wall: set(source.wall()).and_then(|now| wall_clock(now.checked_sub(Self::LEAD)?).ok()),
			source,
		}
	}

	/// Start a clock at an explicit monotonic epoch and wall time: PTS zero at both.
	///
	/// The deterministic constructor: synthetic sources and fixtures pin both ends instead of
	/// sampling. `epoch` is an instant on the ambient clock, which the clock keeps reading wherever
	/// it is read from. Refuses an unrepresentable wall.
	pub fn at(epoch: Instant, wall: Wall) -> crate::Result<Self> {
		let wall = Some(wall_clock(wall)?);
		let source = kio::clock::Clock::current();
		Ok(Self {
			timeline: Timeline::start(&source, epoch, 0),
			source,
			wall,
		})
	}

	/// This clock moved to read `since` now: a source whose first timestamp is `since` is live on
	/// arrival.
	///
	/// Refuses a `since` so large that PTS zero lands before the moq epoch (2020), which the wall
	/// mapping cannot name. On a host whose wall clock is unset, the clock has no wall mapping.
	pub(crate) fn arrival(&self, since: Duration) -> crate::Result<Self> {
		let unmappable = || crate::Error::UnmappableTimestamp(format!("{since:?} puts PTS zero before 2020"));
		let wall = match set(self.source.wall()) {
			Some(now) => {
				let zero = now.checked_sub(since).ok_or_else(unmappable)?;
				Some(wall_clock(zero).map_err(|_| unmappable())?)
			}
			// Past the mapping's range, PTS zero is before 2020 for any host before 2305; refusing it
			// also keeps the readings within a timestamp as the clock advances.
			None if since.as_micros() > MAX_SAFE_INTEGER as u128 => return Err(unmappable()),
			None => None,
		};
		let reading = u64::try_from(since.as_nanos()).map_err(|_| unmappable())?;
		Ok(Self {
			timeline: Timeline::start(&self.source, self.source.now(), reading),
			source: self.source.clone(),
			wall,
		})
	}

	/// This clock moved so `pts` is the instant `wall`: a source that knows when its timestamps
	/// happened places them there rather than at their arrival.
	///
	/// Refuses a mapping whose PTS zero lands before the moq epoch (2020) or after now, and a host
	/// whose wall clock is unset, which has no now to place it against.
	pub(crate) fn placed(&self, pts: Duration, wall: Wall) -> crate::Result<Self> {
		let unmappable = |why: &str| crate::Error::UnmappableTimestamp(format!("{pts:?} at {wall:?} {why}"));
		let zero = wall
			.checked_sub(pts)
			.and_then(|zero| wall_clock(zero).ok())
			.ok_or_else(|| unmappable("puts PTS zero before 2020"))?;
		self.mapped(zero, unmappable)
	}

	/// This clock without its wall mapping: the same timeline, for a broadcast whose source
	/// advertises no `sync.clock`, so none is invented.
	pub(crate) fn unmapped(&self) -> Self {
		Self {
			wall: None,
			..self.clone()
		}
	}

	/// A catalog root `clock` in [`TIMESCALE`](Self::TIMESCALE), the units [`wall`](Self::wall)
	/// reports, so equal mappings compare equal.
	///
	/// Refuses a wall that is no whole number of those units (some 90 kHz values).
	pub(crate) fn exact(wall: hang::catalog::Clock) -> crate::Result<hang::catalog::Clock> {
		let unmappable = |why: &str| crate::Error::UnmappableTimestamp(format!("root clock {wall:?} {why}"));
		let scale = wall
			.wall
			.scale()
			.ok_or_else(|| unmappable("has no timescale"))?
			.as_u64() as u128;
		let scaled = wall.wall.value() as u128 * Self::TIMESCALE.as_u64() as u128;
		if !scaled.is_multiple_of(scale) {
			return Err(unmappable("is no whole microsecond"));
		}
		let micros = u64::try_from(scaled / scale).map_err(|_| unmappable("overflows the clock"))?;
		Ok(hang::catalog::Clock::new(moq_net::Timestamp::from_micros(micros)?)?)
	}

	/// This clock moved onto an [`exact`](Self::exact) catalog root `clock`: a derivative
	/// republishing its input's timestamps runs on its input's mapping.
	///
	/// Refuses a PTS zero after now, and a host whose wall clock is unset.
	pub(crate) fn adopted(&self, wall: hang::catalog::Clock) -> crate::Result<Self> {
		self.mapped(wall, |why| {
			crate::Error::UnmappableTimestamp(format!("root clock {wall:?} {why}"))
		})
	}

	/// This clock moved so its PTS zero is `zero`, anchored now at however long ago that was on the
	/// host.
	///
	/// No `Instant` arithmetic, so a PTS zero before this host booted still maps.
	fn mapped(&self, zero: hang::catalog::Clock, unmappable: impl Fn(&str) -> crate::Error) -> crate::Result<Self> {
		let now = set(self.source.wall()).ok_or_else(|| unmappable("needs the host wall clock, which is unset"))?;
		// Saturating would stamp everything early by the skew without saying so.
		let zero_micros = zero.wall.as_micros().map_err(|_| unmappable("has no timescale"))?;
		let reading =
			u64::try_from(moq_micros(now) - zero_micros as i128).map_err(|_| unmappable("puts PTS zero after now"))?;
		Ok(Self {
			timeline: Timeline::start(&self.source, self.source.now(), reading * 1_000),
			source: self.source.clone(),
			wall: Some(zero),
		})
	}

	/// What this clock reads at `wall`, in microseconds: negative before PTS zero.
	///
	/// Pure in the stored mapping, like [`wall_clock`](Self::wall_clock), and refused without one.
	pub(crate) fn reading(&self, wall: Wall) -> crate::Result<i128> {
		Ok(moq_micros(wall) - self.mapping()?.wall.as_micros()? as i128)
	}

	/// The wall mapping, refused when the broadcast has none.
	fn mapping(&self) -> crate::Result<hang::catalog::Clock> {
		self.wall
			.ok_or_else(|| crate::Error::UnmappableTimestamp("the broadcast has no wall mapping".to_string()))
	}

	/// The current timestamp on this clock.
	pub fn now(&self) -> moq_net::Timestamp {
		let (_, reading) = self.read();
		moq_net::Timestamp::from_micros(reading / 1_000).expect("an instant elapsed duration fits in a timestamp")
	}

	/// The host's instant now, and what the timeline reads at it.
	fn read(&self) -> (Instant, u64) {
		let mut timeline = self.timeline.lock().unwrap_or_else(PoisonError::into_inner);
		// Read under the lock, so racing reads land on the timeline in order.
		let now = self.source.now();
		let reading = timeline.read(now, self.source.suspended(), set(self.source.wall()));
		(now, reading)
	}

	/// The instant now on the ambient clock this one was created under, whichever thread or runtime
	/// asks: what [`capture`](Self::capture) takes, and every re-basing of this clock shares.
	pub(crate) fn instant(&self) -> Instant {
		self.source.now()
	}

	/// Map the instant a payload was captured (a datagram's arrival, a sensor read) onto this clock.
	///
	/// `at` is an instant on the ambient clock this one was created under, which is the host's
	/// outside tests. Refuses an instant ahead of now, which would claim the payload reached the
	/// transport before it existed, and one before PTS zero, which no timestamp can name.
	pub fn capture(&self, at: Instant) -> crate::Result<moq_net::Timestamp> {
		let (now, reading) = self.read();
		let before = now.checked_duration_since(at).ok_or(crate::Error::InvalidCapture)?;
		let nanos = reading
			.checked_sub(before.as_nanos() as u64)
			.ok_or(crate::Error::InvalidCapture)?;
		Ok(moq_net::Timestamp::from_micros(nanos / 1_000).expect("an instant elapsed duration fits in a timestamp"))
	}

	/// Units per second for [`wall`](Self::wall): [`TIMESCALE`](Self::TIMESCALE).
	pub fn timescale(&self) -> moq_net::Timescale {
		Self::TIMESCALE
	}

	/// The catalog root section advertising this clock, or `None` if the broadcast has no wall mapping:
	/// the host's wall clock was unset, or the source advertised none.
	pub fn wall(&self) -> Option<hang::catalog::Clock> {
		self.wall
	}

	/// The wall-clock time of `pts` under this broadcast's mapping.
	///
	/// Pure in the stored mapping: a system-clock adjustment after construction changes nothing.
	/// Exact down to the nanosecond; an instant finer than that (a 90 kHz tick) is floored.
	/// Refuses an unrepresentable result, and a clock without a mapping.
	pub fn wall_clock(&self, pts: moq_net::Timestamp) -> crate::Result<Wall> {
		Ok(self.mapping()?.wall_clock(pts)?)
	}
}

impl PartialEq for Clock {
	fn eq(&self, other: &Self) -> bool {
		Arc::ptr_eq(&self.timeline, &other.timeline)
	}
}

impl Eq for Clock {}

impl Default for Clock {
	fn default() -> Self {
		Self::new()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn epoch() -> Instant {
		kio::clock::now()
	}

	fn moq_epoch() -> Wall {
		Wall::MOQ_EPOCH
	}

	fn us(v: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(v).unwrap()
	}

	/// A capture maps onto the clock's own timeline, and one ahead of now or before the epoch is
	/// refused rather than clamped.
	#[kio::test]
	async fn a_capture_maps_onto_the_clock() {
		kio::clock::sleep(Duration::from_secs(6)).await;
		let now = kio::clock::now();
		let clock = Clock::at(now.saturating_sub(Duration::from_secs(5)), moq_epoch()).unwrap();
		assert_eq!(
			clock.capture(now.saturating_sub(Duration::from_secs(2))).unwrap(),
			us(3_000_000)
		);

		assert!(matches!(
			clock.capture(kio::clock::now().saturating_add(Duration::from_secs(1))),
			Err(crate::Error::InvalidCapture)
		));
		assert!(matches!(
			clock.capture(now.saturating_sub(Duration::from_secs(6))),
			Err(crate::Error::InvalidCapture)
		));
	}

	#[kio::test]
	async fn simulated_clock_preserves_logical_boundaries() {
		kio::clock::sleep(Duration::from_secs(10)).await;
		let epoch = kio::clock::now();
		let clock = Clock::at(epoch, moq_epoch()).unwrap();
		let fresh = Clock::new();
		let before = fresh.now();
		let wall = clock.wall();

		kio::clock::sleep(Duration::from_secs(3)).await;
		assert_eq!(clock.now(), us(3_000_000));
		assert_eq!(
			fresh.now().as_micros().unwrap() - before.as_micros().unwrap(),
			3_000_000
		);
		assert_eq!(clock.wall(), wall);
		assert_eq!(
			clock.wall_clock(clock.now()).unwrap(),
			moq_epoch() + Duration::from_secs(3)
		);

		let stepped = kio::clock::Manual::WALL_START + Duration::from_secs(3600);
		kio::clock::sim::step_wall(stepped);
		assert_eq!(clock.wall(), wall, "a wall correction does not retime archived media");
		assert_eq!(
			Clock::new().wall(),
			wall_clock(stepped - Clock::LEAD).ok(),
			"a new mapping samples the corrected wall"
		);

		let captured = epoch.saturating_add(Duration::from_secs(2));
		assert_eq!(clock.capture(captured).unwrap(), us(2_000_000));
		assert!(matches!(
			clock.capture(epoch.saturating_add(Duration::from_secs(4))),
			Err(crate::Error::InvalidCapture)
		));
		assert!(matches!(
			clock.capture(epoch.saturating_sub(Duration::from_secs(1))),
			Err(crate::Error::InvalidCapture)
		));
	}

	/// How far `wall + now` reads ahead of the host's wall clock, in nanoseconds.
	fn error(clock: &Clock) -> i128 {
		unix_nanos(clock.wall_clock(clock.now()).unwrap()) - unix_nanos(kio::clock::wall())
	}

	/// How often [`run`] reads the clock.
	const TICK: Duration = Duration::from_secs(10);

	/// Let `duration` pass a [`TICK`] at a time, checking that each advances the clock by a tick
	/// give or take the slew, and return the error after it.
	async fn run(clock: &Clock, duration: Duration) -> i128 {
		let tick = TICK.as_micros();
		for _ in 0..duration.as_secs() / TICK.as_secs() {
			let before = clock.now().as_micros().unwrap();
			kio::clock::sleep(TICK).await;
			let step = clock.now().as_micros().unwrap() - before;
			assert!(step.abs_diff(tick) <= tick / SLEW as u128 + 1, "a tick read {step} us");
		}
		error(clock)
	}

	/// The broadcast's wall times stay on the host's wall clock across a day of its monotonic clock
	/// drifting 500 ppm either way, follow a wall step either way at the slew cap without jumping,
	/// and jump forward across a suspend.
	#[kio::test]
	async fn wall_times_follow_the_host_wall_clock() {
		let second = 1_000_000_000;
		let clock = Clock::new();
		let settled = |error: i128| error.abs() <= SETTLED + TICK.as_nanos() as i128 / SLEW as i128;

		for ppm in [500, -500] {
			kio::clock::sim::set_drift(ppm);
			let error = run(&clock, Duration::from_secs(12 * 3600)).await;
			assert!(settled(error), "{ppm} ppm for 12 h left {error} ns");
		}
		kio::clock::sim::set_drift(0);

		// A wall step either way is slewed out at the cap: 10 s takes 20,000 s.
		for step in [10i128, -10] {
			let wall = kio::clock::wall();
			let stepped = match step > 0 {
				true => wall + Duration::from_secs(10),
				false => wall - Duration::from_secs(10),
			};
			kio::clock::sim::step_wall(stepped);
			let error = run(&clock, Duration::from_secs(10_000)).await;
			let half = -step * second / 2;
			assert!(
				(error - half).abs() <= SETTLED,
				"halfway through a {step} s step: {error} ns"
			);
			let error = run(&clock, Duration::from_secs(10_100)).await;
			assert!(settled(error), "after a {step} s step: {error} ns");
		}

		// No media exists while the host sleeps: the clock jumps the gap and stays on the wall clock.
		let before = clock.now().as_micros().unwrap();
		kio::clock::sim::suspend(Duration::from_secs(3600));
		let after = clock.now().as_micros().unwrap();
		assert_eq!(after - before, 3_600_000_000, "a suspend is a gap");
		assert!(settled(error(&clock)));
		assert!(settled(run(&clock, Duration::from_secs(60)).await));
	}

	/// Clones share one timeline, so the slew one applies is the others' too.
	#[kio::test]
	async fn clones_share_the_slew() {
		let clock = Clock::new();
		let clone = clock.clone();
		assert_eq!(clock, clone);
		assert_ne!(clock, Clock::new(), "a new clock is a new timeline");

		kio::clock::sim::step_wall(kio::clock::wall() + Duration::from_secs(5));
		kio::clock::sleep(Duration::from_secs(100)).await;
		let slewed = clock.now();
		assert_eq!(slewed.as_micros().unwrap(), 10_000_000 + 100_000_000 + 50_000);
		assert_eq!(clone.now(), slewed);
	}

	#[test]
	fn section_advertises_wall_in_clock_timescale() {
		// PTS zero at exactly the moq epoch advertises 0.
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		assert_eq!(clock.wall().unwrap().wall.value(), 0);
		assert_eq!(clock.wall().unwrap().wall.scale(), Some(Clock::TIMESCALE));

		// A second later is a second's worth of clock units.
		let clock = Clock::at(epoch(), moq_epoch() + Duration::from_secs(1)).unwrap();
		assert_eq!(clock.wall().unwrap().wall.value(), 1_000_000);
	}

	#[test]
	fn unrepresentable_walls_are_refused() {
		// One micro past the JSON-safe integer range: browsers would read a different number.
		let far = Wall::MOQ_EPOCH + Duration::from_micros(MAX_SAFE_INTEGER + 1);
		assert!(Clock::at(epoch(), far).is_err());

		// Before 2020 cannot be named on the wire; saturating to the epoch would lie.
		assert!(Clock::at(epoch(), Wall::UNIX_EPOCH).is_err());
		assert!(Clock::at(epoch(), moq_epoch() - Duration::from_micros(1)).is_err());
		assert_eq!(Clock::at(epoch(), moq_epoch()).unwrap().wall().unwrap().wall.value(), 0);
	}

	#[test]
	fn fresh_clock_leaves_room_before_now() {
		let clock = Clock::new();
		// PTS zero sits before construction, so a frame stamped a little before now still maps,
		// and the mapping still names the current wall time.
		assert!(clock.now().as_micros().unwrap() >= Clock::LEAD.as_micros());
		let now = clock.wall_clock(clock.now()).unwrap();
		let host = kio::clock::wall();
		let drift = now.max(host).saturating_duration_since(now.min(host));
		assert!(drift < Duration::from_secs(1), "wall + now is the current wall time");
	}

	/// A source whose first timestamp is ten hours in reads that timestamp now, mapped to now.
	#[kio::test]
	async fn arrival_maps_the_first_timestamp_to_now() {
		kio::clock::sleep(Duration::from_secs(1)).await;
		let since = Duration::from_secs(10 * 3600);
		let clock = Clock::new().arrival(since).unwrap();
		let now = clock.now();
		assert!(now.as_micros().unwrap() >= since.as_micros());
		let actual = clock.wall_clock(now).unwrap();
		let host = kio::clock::wall();
		let drift = actual.max(host).saturating_duration_since(actual.min(host));
		assert!(drift < Duration::from_secs(1), "the first timestamp is live on arrival");

		// A capture from before the arrival still maps, down to PTS zero.
		let earlier = clock
			.capture(kio::clock::now().saturating_sub(Duration::from_secs(1)))
			.unwrap();
		assert!(earlier.as_micros().unwrap() < now.as_micros().unwrap());

		// PTS zero before 2020 cannot be named on the wire.
		assert!(matches!(
			Clock::new().arrival(Duration::from_secs(100 * 365 * 86_400)),
			Err(crate::Error::UnmappableTimestamp(_))
		));
	}

	/// A host wall clock before 2020 is unset: the clock keeps its monotonic timeline without a wall
	/// mapping, and refuses whatever needs one rather than invent it.
	#[test]
	fn an_unset_host_clock_has_no_wall() {
		fake_host_wall(Wall::UNIX_EPOCH + Duration::from_secs(86_400));

		let clock = Clock::new();
		assert_eq!(clock.wall(), None);
		assert!(clock.now().as_micros().unwrap() >= Clock::LEAD.as_micros());
		assert!(matches!(
			clock.wall_clock(clock.now()),
			Err(crate::Error::UnmappableTimestamp(_))
		));
		assert!(matches!(
			clock.reading(moq_epoch()),
			Err(crate::Error::UnmappableTimestamp(_))
		));

		// An importer arriving an hour in still gets a timeline, and placing one has no wall to use.
		assert_eq!(clock.arrival(Duration::from_secs(3600)).unwrap().wall(), None);
		assert!(matches!(
			clock.arrival(Duration::from_micros(MAX_SAFE_INTEGER + 1)),
			Err(crate::Error::UnmappableTimestamp(_))
		));
		assert!(matches!(
			clock.placed(Duration::from_secs(1), moq_epoch() + Duration::from_secs(60)),
			Err(crate::Error::UnmappableTimestamp(_))
		));

		// An explicit wall needs no host clock.
		assert!(Clock::at(epoch(), moq_epoch()).unwrap().wall().is_some());
	}

	#[test]
	fn wall_mapping_survives_a_system_clock_adjustment() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		let before = clock.wall_clock(us(2_000_000)).unwrap();

		// The mapping is a stored epoch, not a sampled clock: reading it again (after whatever
		// the system clock did in between) changes nothing, and retained records map identically.
		assert_eq!(clock.wall_clock(us(2_000_000)).unwrap(), before);
		assert_eq!(
			before,
			moq_epoch() + Duration::from_secs(2),
			"archive records keep their wall times"
		);
	}

	/// Regression: a wall with a sub-millisecond part came back up to 999 µs early.
	#[test]
	fn wall_clock_roundtrips_a_microsecond_wall() {
		let wall = moq_epoch() + Duration::from_micros(1_000_999);
		let clock = Clock::at(epoch(), wall).unwrap();
		assert_eq!(clock.wall_clock(moq_net::Timestamp::ZERO).unwrap(), wall);
	}

	#[test]
	fn wall_clock_validates_bounds() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		// The largest representable broadcast timestamp still maps.
		let max = moq_net::Timestamp::from_micros((1u64 << 61) - 1).unwrap();
		assert!(clock.wall_clock(max).is_err(), "past the JSON-safe range is refused");
	}
}
