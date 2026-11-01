//! The broadcast's shared clock: one monotonic timeline plus its wall mapping.
//!
//! Create one [`Clock`] per broadcast and hand copies to every producer: because they share a
//! timeline, frames captured at the same instant get the same timestamp, keeping concurrently
//! produced tracks (e.g. audio and video capture on separate threads) in sync. It is `Copy`, so
//! handing it out is cheap. Taking the catalog's clock fixes it, so a copy never goes stale.
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
use std::time::Duration;

use hang::catalog::{MAX_SAFE_INTEGER, MOQ_EPOCH_UNIX_MILLIS};
use kio::clock::Instant;

/// Microseconds from the moq epoch (2020) to `wall`, negative before it.
fn moq_micros(wall: Wall) -> i128 {
	let moq_epoch = MOQ_EPOCH_UNIX_MILLIS as i128 * 1000;
	wall.saturating_duration_since(Wall::UNIX_EPOCH).as_micros() as i128 - moq_epoch
}

/// The host's wall clock, or `None` while it reads before the moq epoch (2020): unset.
pub(crate) fn host_wall() -> Option<Wall> {
	let now = kio::clock::wall();
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

/// A monotonic clock for stamping media frames so that tracks produced
/// concurrently, e.g. an audio and a video capture running on separate
/// threads, land on a single timeline.
///
/// Copies share the timeline and the wall mapping, so handing them to several producers keeps
/// the broadcast on one clock. Two clocks are equal only when they are one timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
	/// A monotonic instant, and what the clock read then in micros.
	instant: Instant,
	reading: u64,
	/// `None` when the host's wall clock was unset at construction.
	wall: Option<hang::catalog::Clock>,
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
		Self {
			instant: kio::clock::now(),
			reading: Self::LEAD.as_micros() as u64,
			wall: host_wall().and_then(|now| wall_clock(now.checked_sub(Self::LEAD)?).ok()),
		}
	}

	/// Start a clock at an explicit monotonic epoch and wall time: PTS zero at both.
	///
	/// The deterministic constructor: synthetic sources and fixtures pin both ends instead of
	/// sampling. Refuses an unrepresentable wall.
	pub fn at(epoch: Instant, wall: Wall) -> crate::Result<Self> {
		Ok(Self {
			instant: epoch,
			reading: 0,
			wall: Some(wall_clock(wall)?),
		})
	}

	/// A clock that reads `since` now: a source whose first timestamp is `since` is live on arrival.
	///
	/// Refuses a `since` so large that PTS zero lands before the moq epoch (2020), which the wall
	/// mapping cannot name. On a host whose wall clock is unset, the clock has no wall mapping.
	pub(crate) fn arrival(since: Duration) -> crate::Result<Self> {
		let instant = kio::clock::now();
		let unmappable = || crate::Error::UnmappableTimestamp(format!("{since:?} puts PTS zero before 2020"));
		let wall = match host_wall() {
			Some(now) => {
				let zero = now.checked_sub(since).ok_or_else(unmappable)?;
				Some(wall_clock(zero).map_err(|_| unmappable())?)
			}
			// Past the mapping's range, PTS zero is before 2020 for any host before 2305; refusing it
			// also keeps the readings within a timestamp as the clock advances.
			None if since.as_micros() > MAX_SAFE_INTEGER as u128 => return Err(unmappable()),
			None => None,
		};
		Ok(Self {
			instant,
			reading: u64::try_from(since.as_micros()).map_err(|_| unmappable())?,
			wall,
		})
	}

	/// A clock on which `pts` is the instant `wall`: a source that knows when its timestamps
	/// happened places them there rather than at their arrival.
	///
	/// Refuses a mapping whose PTS zero lands before the moq epoch (2020) or after now, and a host
	/// whose wall clock is unset, which has no now to place it against.
	pub(crate) fn placed(pts: Duration, wall: Wall) -> crate::Result<Self> {
		let unmappable = |why: &str| crate::Error::UnmappableTimestamp(format!("{pts:?} at {wall:?} {why}"));
		let zero = wall
			.checked_sub(pts)
			.and_then(|zero| wall_clock(zero).ok())
			.ok_or_else(|| unmappable("puts PTS zero before 2020"))?;
		Self::mapped(zero, unmappable)
	}

	/// A catalog root `clock` in [`TIMESCALE`](Self::TIMESCALE), the units [`wall`](Self::wall)
	/// reports, so equal mappings compare equal.
	///
	/// Refuses a wall that is no whole number of those units (some 90 kHz values).
	pub(crate) fn exact(wall: hang::catalog::Clock) -> crate::Result<hang::catalog::Clock> {
		let unmappable = |why: &str| crate::Error::UnmappableTimestamp(format!("root clock {wall:?} {why}"));
		let scale = wall.wall.scale().as_u64() as u128;
		let scaled = wall.wall.value() as u128 * Self::TIMESCALE.as_u64() as u128;
		if !scaled.is_multiple_of(scale) {
			return Err(unmappable("is no whole microsecond"));
		}
		let micros = u64::try_from(scaled / scale).map_err(|_| unmappable("overflows the clock"))?;
		Ok(hang::catalog::Clock::new(moq_net::Timestamp::from_micros(micros)?)?)
	}

	/// A clock on an [`exact`](Self::exact) catalog root `clock`: a derivative republishing its
	/// input's timestamps runs on its input's mapping.
	///
	/// Refuses a PTS zero after now, and a host whose wall clock is unset.
	pub(crate) fn adopted(wall: hang::catalog::Clock) -> crate::Result<Self> {
		Self::mapped(wall, |why| {
			crate::Error::UnmappableTimestamp(format!("root clock {wall:?} {why}"))
		})
	}

	/// A clock whose PTS zero is `zero`, anchored now at however long ago that was on the host.
	///
	/// No `Instant` arithmetic, so a PTS zero before this host booted still maps.
	fn mapped(zero: hang::catalog::Clock, unmappable: impl Fn(&str) -> crate::Error) -> crate::Result<Self> {
		let instant = kio::clock::now();
		let now = host_wall().ok_or_else(|| unmappable("needs the host wall clock, which is unset"))?;
		// Saturating would stamp everything early by the skew without saying so.
		let reading = u64::try_from(moq_micros(now) - zero.wall.as_micros() as i128)
			.map_err(|_| unmappable("puts PTS zero after now"))?;
		Ok(Self {
			instant,
			reading,
			wall: Some(zero),
		})
	}

	/// What this clock reads at `wall`, in microseconds: negative before PTS zero.
	///
	/// Pure in the stored mapping, like [`wall_clock`](Self::wall_clock), and refused without one.
	pub(crate) fn reading(&self, wall: Wall) -> crate::Result<i128> {
		Ok(moq_micros(wall) - self.mapping()?.wall.as_micros() as i128)
	}

	/// The wall mapping, refused when the host's wall clock was unset.
	fn mapping(&self) -> crate::Result<hang::catalog::Clock> {
		self.wall
			.ok_or_else(|| crate::Error::UnmappableTimestamp("the host wall clock was unset".to_string()))
	}

	/// The current timestamp on this clock.
	pub fn now(&self) -> moq_net::Timestamp {
		// u128 -> u64 truncation is unreachable: u64 microseconds is ~584,000 years.
		let elapsed = self.instant.elapsed().as_micros() as u64;
		moq_net::Timestamp::from_micros(self.reading + elapsed)
			.expect("an instant elapsed duration fits in a timestamp")
	}

	/// Map the instant a payload was captured (a datagram's arrival, a sensor read) onto this clock.
	///
	/// Refuses an instant ahead of now, which would claim the payload reached the transport before
	/// it existed, and one before PTS zero, which no timestamp can name.
	pub fn capture(&self, at: Instant) -> crate::Result<moq_net::Timestamp> {
		if at > kio::clock::now() {
			return Err(crate::Error::InvalidCapture);
		}
		let micros = match at.checked_duration_since(self.instant) {
			Some(after) => self.reading + after.as_micros() as u64,
			None => self
				.reading
				.checked_sub(self.instant.duration_since(at).as_micros() as u64)
				.ok_or(crate::Error::InvalidCapture)?,
		};
		Ok(moq_net::Timestamp::from_micros(micros).expect("an instant elapsed duration fits in a timestamp"))
	}

	/// Units per second for [`wall`](Self::wall): [`TIMESCALE`](Self::TIMESCALE).
	pub fn timescale(&self) -> moq_net::Timescale {
		Self::TIMESCALE
	}

	/// The catalog root section advertising this clock, or `None` if the host's wall clock was unset.
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
		Wall::UNIX_EPOCH + Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS)
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
		let clock = Clock::at(now - Duration::from_secs(5), moq_epoch()).unwrap();
		assert_eq!(clock.capture(now - Duration::from_secs(2)).unwrap(), us(3_000_000));

		assert!(matches!(
			clock.capture(kio::clock::now() + Duration::from_secs(1)),
			Err(crate::Error::InvalidCapture)
		));
		assert!(matches!(
			clock.capture(now - Duration::from_secs(6)),
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
		assert_eq!(fresh.now().as_micros() - before.as_micros(), 3_000_000);
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

		let captured = epoch + Duration::from_secs(2);
		assert_eq!(clock.capture(captured).unwrap(), us(2_000_000));
		assert!(matches!(
			clock.capture(epoch + Duration::from_secs(4)),
			Err(crate::Error::InvalidCapture)
		));
		assert!(matches!(
			clock.capture(epoch - Duration::from_secs(1)),
			Err(crate::Error::InvalidCapture)
		));
	}

	#[test]
	fn section_advertises_wall_in_clock_timescale() {
		// PTS zero at exactly the moq epoch advertises 0.
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		assert_eq!(clock.wall().unwrap().wall.value(), 0);
		assert_eq!(clock.wall().unwrap().wall.scale(), Clock::TIMESCALE);

		// A second later is a second's worth of clock units.
		let clock = Clock::at(epoch(), moq_epoch() + Duration::from_secs(1)).unwrap();
		assert_eq!(clock.wall().unwrap().wall.value(), 1_000_000);
	}

	#[test]
	fn unrepresentable_walls_are_refused() {
		// One micro past the JSON-safe integer range: browsers would read a different number.
		let past_safe = MOQ_EPOCH_UNIX_MILLIS * 1000 + MAX_SAFE_INTEGER + 1;
		let far = Wall::UNIX_EPOCH + Duration::from_micros(past_safe);
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
		assert!(clock.now().as_micros() >= Clock::LEAD.as_micros());
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
		let clock = Clock::arrival(since).unwrap();
		let now = clock.now();
		assert!(now.as_micros() >= since.as_micros());
		let actual = clock.wall_clock(now).unwrap();
		let host = kio::clock::wall();
		let drift = actual.max(host).saturating_duration_since(actual.min(host));
		assert!(drift < Duration::from_secs(1), "the first timestamp is live on arrival");

		// A capture from before the arrival still maps, down to PTS zero.
		let earlier = clock.capture(kio::clock::now() - Duration::from_secs(1)).unwrap();
		assert!(earlier.as_micros() < now.as_micros());

		// PTS zero before 2020 cannot be named on the wire.
		assert!(matches!(
			Clock::arrival(Duration::from_secs(100 * 365 * 86_400)),
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
		assert!(clock.now().as_micros() >= Clock::LEAD.as_micros());
		assert!(matches!(
			clock.wall_clock(clock.now()),
			Err(crate::Error::UnmappableTimestamp(_))
		));
		assert!(matches!(
			clock.reading(moq_epoch()),
			Err(crate::Error::UnmappableTimestamp(_))
		));

		// An importer arriving an hour in still gets a timeline, and placing one has no wall to use.
		assert_eq!(Clock::arrival(Duration::from_secs(3600)).unwrap().wall(), None);
		assert!(matches!(
			Clock::arrival(Duration::from_micros(MAX_SAFE_INTEGER + 1)),
			Err(crate::Error::UnmappableTimestamp(_))
		));
		assert!(matches!(
			Clock::placed(Duration::from_secs(1), moq_epoch() + Duration::from_secs(60)),
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
