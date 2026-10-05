//! The broadcast's shared clock: one monotonic timeline plus its wall mapping.
//!
//! Create one [`Clock`] per broadcast and hand copies to every producer: because they share a
//! timeline, frames captured at the same instant get the same timestamp, keeping concurrently
//! produced tracks (e.g. audio and video capture on separate threads) in sync. It is `Copy`, so
//! handing it out is cheap. A copy is a snapshot, though: beside a container importer, which
//! re-anchors the catalog's clock on its first frame, read the catalog's clock at write time instead.
//!
//! The clock also owns the broadcast's wall mapping, advertised at the catalog root as
//! `clock: { wall, timescale }`: `wall` is the wall-clock time of PTS zero in
//! [`Clock::TIMESCALE`] units since the moq epoch (2020-01-01). A consumer derives any
//! timestamp's wall time as `wall + pts` after converting it into this timescale. A discontinuity
//! marker is a delivery event, not a new epoch, and a system-clock adjustment never retimes it.
//!
//! A container importer publishes its stream's own timestamps, so it places the mapping instead:
//! its first timestamp is live on arrival (see [`Config::with_clock`](crate::catalog::Config::with_clock)).

use std::time::{Duration, Instant, SystemTime};

use hang::catalog::{MAX_SAFE_INTEGER, MOQ_EPOCH_UNIX_MILLIS};

/// The catalog clock for PTS zero at `wall`.
fn wall_clock(wall: SystemTime) -> crate::Result<hang::catalog::Clock> {
	let unix_micros = wall
		.duration_since(SystemTime::UNIX_EPOCH)
		.map(|d| d.as_micros())
		.map_err(|_| hang::Error::InvalidWall(0))?;
	let moq_epoch_micros = MOQ_EPOCH_UNIX_MILLIS as u128 * 1000;
	// A time before 2020 cannot be named on the wire; refuse it rather than advertise 2020.
	if unix_micros < moq_epoch_micros {
		return Err(hang::Error::InvalidWall(0).into());
	}
	let wall = u64::try_from(unix_micros - moq_epoch_micros).unwrap_or(u64::MAX);
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
/// the broadcast on one clock.
///
/// A copy goes stale when a container importer's first frame re-anchors the catalog's clock (see
/// [`Config::with_clock`](crate::catalog::Config::with_clock)): it keeps mapping onto the old
/// timeline, misaligning everything it captures. To [`capture`](Self::capture) beside an importer,
/// use [`catalog::Producer::clock`](crate::catalog::Producer::clock) read at write time.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
	/// A monotonic instant, and what the clock read then in micros.
	instant: Instant,
	reading: u64,
	wall: hang::catalog::Clock,
}

impl Clock {
	/// Units per second for the broadcast clock: microseconds.
	///
	/// Matches the hang container timescale, so media timestamps convert without loss, and fine
	/// enough that the wall value stays within the JSON-safe integer range until the year 2255.
	pub const TIMESCALE: moq_net::Timescale = moq_net::Timescale::MICRO;

	/// What a fresh clock reads at construction.
	///
	/// Frames stamped beside it can carry slightly earlier timestamps (audio captured a moment
	/// before the video it is muxed with). Starting the clock this far past zero leaves them room
	/// instead of landing before the broadcast began.
	const LEAD: Duration = Duration::from_secs(10);

	/// Start a clock that reads ten seconds now, so PTS zero is ten seconds ago on the wall.
	pub fn new() -> Self {
		Self::arrival(Self::LEAD).expect("the current wall time is representable as a broadcast clock")
	}

	/// Start a clock at an explicit monotonic epoch and wall time: PTS zero at both.
	///
	/// The deterministic constructor: synthetic sources and fixtures pin both ends instead of
	/// sampling. Refuses an unrepresentable wall.
	pub fn at(epoch: Instant, wall: SystemTime) -> crate::Result<Self> {
		Ok(Self {
			instant: epoch,
			reading: 0,
			wall: wall_clock(wall)?,
		})
	}

	/// A clock that reads `since` now: a source whose first timestamp is `since` is live on arrival.
	///
	/// Refuses a `since` so large that PTS zero lands before the moq epoch (2020), which the wall
	/// mapping cannot name.
	pub(crate) fn arrival(since: Duration) -> crate::Result<Self> {
		let (instant, now) = (Instant::now(), SystemTime::now());
		let unmappable = || crate::Error::UnmappableTimestamp(format!("{since:?} puts PTS zero before 2020"));
		let zero = now.checked_sub(since).ok_or_else(unmappable)?;
		Ok(Self {
			instant,
			reading: u64::try_from(since.as_micros()).map_err(|_| unmappable())?,
			wall: wall_clock(zero).map_err(|_| unmappable())?,
		})
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
		if at > Instant::now() {
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

	/// The catalog root section advertising this clock.
	pub fn wall(&self) -> hang::catalog::Clock {
		self.wall
	}

	/// The wall-clock time of `pts` under this broadcast's mapping.
	///
	/// Pure in the stored mapping: a system-clock adjustment after construction changes nothing.
	/// Refuses an unrepresentable result rather than truncating it.
	pub fn wall_clock(&self, pts: moq_net::Timestamp) -> crate::Result<SystemTime> {
		self.wall.wall_clock(pts).map_err(crate::Error::from)
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
		Instant::now()
	}

	fn moq_epoch() -> SystemTime {
		SystemTime::UNIX_EPOCH + Duration::from_millis(MOQ_EPOCH_UNIX_MILLIS)
	}

	fn us(v: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(v).unwrap()
	}

	#[test]
	fn copies_share_one_epoch() {
		let clock = Clock::at(epoch(), moq_epoch() + Duration::from_secs(1)).unwrap();
		let shared = clock;
		// Compare the anchors, not live readings: two `now()` calls race the clock.
		assert_eq!(clock.instant, shared.instant);
		assert_eq!(clock.wall(), shared.wall());
	}

	/// A capture maps onto the clock's own timeline, and one ahead of now or before the epoch is
	/// refused rather than clamped.
	#[test]
	fn a_capture_maps_onto_the_clock() {
		let now = Instant::now();
		let clock = Clock::at(now - Duration::from_secs(5), moq_epoch()).unwrap();
		assert_eq!(clock.capture(now - Duration::from_secs(2)).unwrap(), us(3_000_000));

		assert!(matches!(
			clock.capture(Instant::now() + Duration::from_secs(1)),
			Err(crate::Error::InvalidCapture)
		));
		assert!(matches!(
			clock.capture(now - Duration::from_secs(6)),
			Err(crate::Error::InvalidCapture)
		));
	}

	#[test]
	fn section_advertises_wall_in_clock_timescale() {
		// PTS zero at exactly the moq epoch advertises 0.
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		assert_eq!(clock.wall().wall.value(), 0);
		assert_eq!(clock.wall().wall.scale(), Clock::TIMESCALE);

		// A second later is a second's worth of clock units.
		let clock = Clock::at(epoch(), moq_epoch() + Duration::from_secs(1)).unwrap();
		assert_eq!(clock.wall().wall.value(), 1_000_000);
	}

	#[test]
	fn unrepresentable_walls_are_refused() {
		// One micro past the JSON-safe integer range: browsers would read a different number.
		let past_safe = MOQ_EPOCH_UNIX_MILLIS * 1000 + MAX_SAFE_INTEGER + 1;
		let far = SystemTime::UNIX_EPOCH + Duration::from_micros(past_safe);
		assert!(Clock::at(epoch(), far).is_err());

		// Before 2020 cannot be named on the wire; saturating to the epoch would lie.
		assert!(Clock::at(epoch(), SystemTime::UNIX_EPOCH).is_err());
		assert!(Clock::at(epoch(), moq_epoch() - Duration::from_micros(1)).is_err());
		assert_eq!(Clock::at(epoch(), moq_epoch()).unwrap().wall().wall.value(), 0);
	}

	#[test]
	fn fresh_clock_leaves_room_before_now() {
		let clock = Clock::new();
		// PTS zero sits before construction, so a frame stamped a little before now still maps,
		// and the mapping still names the current wall time.
		assert!(clock.now().as_micros() >= Clock::LEAD.as_micros());
		let now = clock.wall_clock(clock.now()).unwrap();
		let drift = now
			.duration_since(SystemTime::now())
			.unwrap_or_else(|err| err.duration());
		assert!(drift < Duration::from_secs(1), "wall + now is the current wall time");
	}

	/// A source whose first timestamp is ten hours in reads that timestamp now, mapped to now.
	#[test]
	fn arrival_maps_the_first_timestamp_to_now() {
		let since = Duration::from_secs(10 * 3600);
		let clock = Clock::arrival(since).unwrap();
		let now = clock.now();
		assert!(now.as_micros() >= since.as_micros());
		let drift = clock
			.wall_clock(now)
			.unwrap()
			.duration_since(SystemTime::now())
			.unwrap_or_else(|err| err.duration());
		assert!(drift < Duration::from_secs(1), "the first timestamp is live on arrival");

		// A capture from before the arrival still maps, down to PTS zero.
		let earlier = clock.capture(Instant::now() - Duration::from_secs(1)).unwrap();
		assert!(earlier.as_micros() < now.as_micros());

		// PTS zero before 2020 cannot be named on the wire.
		assert!(matches!(
			Clock::arrival(Duration::from_secs(100 * 365 * 86_400)),
			Err(crate::Error::UnmappableTimestamp(_))
		));
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

	#[test]
	fn wall_clock_validates_bounds() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		// The largest representable broadcast timestamp still maps.
		let max = moq_net::Timestamp::from_micros((1u64 << 62) - 1).unwrap();
		assert!(clock.wall_clock(max).is_err(), "past the JSON-safe range is refused");
	}
}
