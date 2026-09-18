//! The broadcast's shared clock: one monotonic epoch plus its fixed wall mapping.
//!
//! Create one [`Clock`] per broadcast and hand copies to every producer: because they share an
//! epoch, frames captured at the same instant get the same timestamp, keeping concurrently
//! produced tracks (e.g. audio and video capture on separate threads) in sync. It is `Copy`, so
//! handing it out is cheap.
//!
//! The clock also owns the broadcast's wall mapping, advertised at the catalog root as
//! `clock: { wall, timescale }`: `wall` is the wall-clock time of PTS zero in
//! [`Clock::TIMESCALE`] units since the moq epoch (2020-01-01). A consumer derives any
//! timestamp's wall time as `wall + pts` after converting it into this timescale. The mapping
//! is fixed at construction and never overwritten: a discontinuity marker is a delivery event,
//! not a new epoch, and a system-clock adjustment never retimes it.
//!
//! A source with its own zero (a file, a restarted encoder) is translated onto the mapping with
//! [`SourceMap`]: the first frame anchors onto the live edge, and a reset re-anchors forward,
//! preserving the real idle gap measured on this monotonic clock.

use std::time::{Duration, Instant, SystemTime};

use hang::catalog::{MAX_SAFE_INTEGER, MOQ_EPOCH_UNIX_MILLIS};

/// The wall-clock time of PTS zero, in [`Clock::TIMESCALE`] units since the moq epoch.
fn wall_units(wall: SystemTime) -> crate::Result<u64> {
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
	Ok(wall)
}

/// A monotonic clock for stamping media frames so that tracks produced
/// concurrently, e.g. an audio and a video capture running on separate
/// threads, land on a single timeline.
///
/// Copies share the epoch and the fixed wall mapping, so handing them to several producers keeps
/// the broadcast on one clock. The wall mapping is established at construction: sampling it when
/// a delayed first frame arrives would pretend that frame is timestamp zero, so the epoch is
/// pinned up front and that frame's PTS converts into the mapping instead.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
	epoch: Instant,
	wall: u64,
}

impl Clock {
	/// Units per second for the broadcast clock: microseconds.
	///
	/// Matches the hang container timescale, so media timestamps convert without loss, and fine
	/// enough that the wall value stays within the JSON-safe integer range until the year 2255.
	pub const TIMESCALE: moq_net::Timescale = moq_net::Timescale::MICRO;

	/// Start a clock anchored at the current instant, with PTS zero at the current wall time.
	pub fn new() -> Self {
		Self::new_at(Instant::now(), SystemTime::now())
			.expect("the current wall time is representable as a broadcast clock")
	}

	/// Start a clock anchored at the current instant, with PTS zero at `wall`.
	///
	/// For an import whose content carries its own start (a recording): the media keeps its
	/// relative spacing and that start names the wall epoch. Refuses a wall before the moq
	/// epoch or outside the JSON-safe integer range rather than publishing a fabricated mapping.
	pub fn with_wall(wall: SystemTime) -> crate::Result<Self> {
		Ok(Self {
			epoch: Instant::now(),
			wall: wall_units(wall)?,
		})
	}

	/// Start a clock at an explicit monotonic epoch and wall time.
	///
	/// The deterministic constructor: synthetic sources and fixtures pin both ends instead of
	/// sampling. Refuses an unrepresentable wall like [`with_wall`](Self::with_wall).
	pub fn new_at(epoch: Instant, wall: SystemTime) -> crate::Result<Self> {
		Ok(Self {
			epoch,
			wall: wall_units(wall)?,
		})
	}

	/// Microseconds elapsed since the clock's epoch.
	pub fn micros(&self) -> u64 {
		// u128 -> u64 truncation is unreachable: u64 microseconds is ~584,000 years.
		self.epoch.elapsed().as_micros() as u64
	}

	/// Units per second for [`wall`](Self::wall): [`TIMESCALE`](Self::TIMESCALE).
	pub fn timescale(&self) -> moq_net::Timescale {
		Self::TIMESCALE
	}

	/// The wall-clock time of PTS zero, in [`TIMESCALE`](Self::TIMESCALE) units since the moq epoch.
	pub fn wall(&self) -> u64 {
		self.wall
	}

	/// The catalog root section advertising this clock: `clock: { wall, timescale }`.
	pub fn section(&self) -> hang::catalog::Clock {
		hang::catalog::Clock::with_timescale(self.wall, Self::TIMESCALE.as_u64() as u32)
			.expect("a constructed clock is always representable")
	}

	/// The wall-clock time of `pts` under this broadcast's fixed mapping.
	///
	/// Pure in the stored epoch: a system-clock adjustment after construction changes nothing.
	/// Refuses an unrepresentable result rather than truncating it.
	pub fn wall_clock(&self, pts: moq_net::Timestamp) -> crate::Result<SystemTime> {
		let scale = u32::try_from(pts.scale().as_u64())
			.map_err(|_| crate::Error::UnmappableTimestamp(format!("timescale {} exceeds u32", pts.scale())))?;
		self.section()
			.wall_clock(pts.value(), scale)
			.map_err(crate::Error::from)
	}

	/// Translate a source with its own zero onto this broadcast's mapping.
	///
	/// Each adapter owns one per source; see [`SourceMap`]. The mapping itself is untouched.
	pub fn source(&self) -> SourceMap {
		SourceMap::new(*self)
	}
}

impl Default for Clock {
	fn default() -> Self {
		Self::new()
	}
}

/// Translates one source's timestamps onto the broadcast clock.
///
/// A source numbers from its own zero (a file starts at its first PTS, an encoder restarts at
/// zero), while the broadcast numbers from the shared epoch. The first frame anchors onto the
/// live edge, preserving the source's spacing from there on; a reset re-anchors forward,
/// preserving the real idle gap measured on the broadcast's monotonic clock. Backwards steps
/// within [`MAX_REORDER`](Self::MAX_REORDER) keep their offset, so permitted B-frame reordering
/// inside a group survives verbatim.
///
/// The broadcast wall mapping is never touched: translating a reset is not a new epoch, and a
/// discontinuity marker the adapter emits alongside is a delivery event the playhead reacts to,
/// not a clock the catalog republishes. Retained records keep their timestamps.
///
/// Each publisher adapter owns one per source and wires its own restart detection to
/// [`reset`](Self::reset); the automatic path only separates reordering from resets by size.
pub struct SourceMap {
	clock: Clock,
	/// Broadcast micros minus source micros; `None` until the first frame anchors it.
	offset: Option<i128>,
	last_source: Option<u128>,
	last_broadcast: Option<u64>,
	/// `clock.micros()` when the last frame was translated: the idle gap's start.
	last_arrival: Option<u64>,
}

impl SourceMap {
	/// The largest backwards source step still treated as permitted in-group reordering
	/// (B-frames present out of decode order) rather than a source reset.
	///
	/// A larger backwards step re-anchors the source forward instead. Adapters that detect a
	/// restart out of band re-anchor explicitly with [`reset`](Self::reset); this bound only
	/// separates the two for the automatic path.
	pub const MAX_REORDER: Duration = Duration::from_millis(500);

	/// A translator onto `clock`, unanchored until the first frame.
	pub fn new(clock: Clock) -> Self {
		Self {
			clock,
			offset: None,
			last_source: None,
			last_broadcast: None,
			last_arrival: None,
		}
	}

	/// The broadcast clock this source translates onto.
	pub fn clock(&self) -> Clock {
		self.clock
	}

	/// Translate `pts` onto the broadcast clock, sampling the arrival time.
	pub fn translate(&mut self, pts: moq_net::Timestamp) -> crate::Result<moq_net::Timestamp> {
		self.translate_at(pts, self.clock.micros())
	}

	/// Translate `pts` onto the broadcast clock, arriving at monotonic `now` micros.
	///
	/// The deterministic core behind [`translate`](Self::translate): synthetic sources pin the
	/// arrival instants instead of sampling them.
	pub fn translate_at(&mut self, pts: moq_net::Timestamp, now: u64) -> crate::Result<moq_net::Timestamp> {
		let src = pts.as_micros();

		let broadcast = match self.offset {
			Some(offset) => {
				let mapped = src as i128 + offset;
				if mapped < 0 {
					return Err(crate::Error::UnmappableTimestamp(format!(
						"{pts:?} lands before the broadcast began"
					)));
				}
				let mapped = u64::try_from(mapped).map_err(|_| {
					crate::Error::UnmappableTimestamp(format!("{pts:?} lands outside the representable range"))
				})?;
				match self.last_broadcast {
					Some(last) if mapped < last && last - mapped > Self::MAX_REORDER.as_micros() as u64 => {
						// A source reset: re-anchor forward, counting the downtime as content.
						self.reanchor(src, now)?
					}
					// Forward, steady, or reordered within a group: the offset stands.
					_ => mapped,
				}
			}
			// The first frame is live now; the source keeps its spacing from there. Rebasing by
			// the frame's own PTS (rather than pretending it is timestamp zero) is what keeps a
			// delayed first frame honest.
			None => {
				self.offset = Some(now as i128 - src as i128);
				now
			}
		};

		self.last_source = Some(src);
		self.last_broadcast = Some(broadcast);
		self.last_arrival = Some(now);
		moq_net::Timestamp::from_micros(broadcast).map_err(crate::Error::from)
	}

	/// Re-anchor after an explicitly detected source restart, preserving the idle gap.
	///
	/// The adapter path for a restart it observes out of band (an encoder reload, a file loop):
	/// the next frame continues after everything published so far plus the downtime since the
	/// previous frame, instead of rewinding the broadcast.
	pub fn reset(&mut self, pts: moq_net::Timestamp) -> crate::Result<moq_net::Timestamp> {
		self.reset_at(pts, self.clock.micros())
	}

	/// [`reset`](Self::reset) with an explicit arrival instant, for synthetic sources.
	pub fn reset_at(&mut self, pts: moq_net::Timestamp, now: u64) -> crate::Result<moq_net::Timestamp> {
		let src = pts.as_micros();
		let broadcast = self.reanchor(src, now)?;
		self.last_source = Some(src);
		self.last_broadcast = Some(broadcast);
		self.last_arrival = Some(now);
		moq_net::Timestamp::from_micros(broadcast).map_err(crate::Error::from)
	}

	/// Move the offset so `src` continues after the last broadcast plus the idle gap since the
	/// previous arrival. Returns the rebased broadcast micros.
	fn reanchor(&mut self, src: u128, now: u64) -> crate::Result<u64> {
		let base = match (self.last_broadcast, self.last_arrival) {
			(Some(last), Some(arrival)) => last as u128 + now.saturating_sub(arrival) as u128,
			// Unanchored: the reset frame itself is live now.
			_ => now as u128,
		};
		self.offset = Some(base as i128 - src as i128);
		let broadcast =
			u64::try_from(base).map_err(|_| crate::Error::UnmappableTimestamp(format!("{base} is out of range")))?;
		// Refuse a mapping that contradicts the range instead of publishing it.
		moq_net::Timestamp::from_micros(broadcast)?;
		Ok(broadcast)
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
		let clock = Clock::new_at(epoch(), moq_epoch() + Duration::from_secs(1)).unwrap();
		let shared = clock;
		// Compare the anchors, not live readings: two `micros()` calls race the clock.
		assert_eq!(clock.epoch, shared.epoch);
		assert_eq!(clock.wall(), shared.wall());
	}

	#[test]
	fn section_advertises_wall_in_clock_timescale() {
		// PTS zero at exactly the moq epoch advertises 0.
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		assert_eq!(clock.section().wall, 0);
		assert_eq!(clock.wall(), 0);
		assert_eq!(clock.section().timescale, Clock::TIMESCALE.as_u64() as u32);

		// A second later is a second's worth of clock units.
		let clock = Clock::new_at(epoch(), moq_epoch() + Duration::from_secs(1)).unwrap();
		assert_eq!(clock.wall(), 1_000_000);
	}

	#[test]
	fn unrepresentable_walls_are_refused() {
		// One micro past the JSON-safe integer range: browsers would read a different number.
		let past_safe = MOQ_EPOCH_UNIX_MILLIS * 1000 + MAX_SAFE_INTEGER + 1;
		let far = SystemTime::UNIX_EPOCH + Duration::from_micros(past_safe);
		assert!(Clock::with_wall(far).is_err());
		assert!(Clock::new_at(epoch(), far).is_err());

		// Before 2020 cannot be named on the wire; saturating to the epoch would lie.
		assert!(Clock::with_wall(SystemTime::UNIX_EPOCH).is_err());
		assert!(Clock::new_at(epoch(), SystemTime::UNIX_EPOCH).is_err());
		assert!(Clock::with_wall(moq_epoch() - Duration::from_micros(1)).is_err());
		assert_eq!(Clock::new_at(epoch(), moq_epoch()).unwrap().wall(), 0);
	}

	#[test]
	fn delayed_first_frame_anchors_live() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();

		// The source's first frame already carries 5s of PTS; it is live now, not now + 5s.
		let first = source.translate_at(us(5_000_000), 100_000).unwrap();
		assert_eq!(first.as_micros(), 100_000);

		// Spacing survives: a second later on the source is a second later on the broadcast.
		let second = source.translate_at(us(6_000_000), 1_100_000).unwrap();
		assert_eq!(second.as_micros(), 1_100_000);

		// And the wall mapping accounts for the PTS: broadcast 100ms is wall + 100ms.
		assert_eq!(
			clock.wall_clock(first).unwrap(),
			moq_epoch() + Duration::from_micros(100_000)
		);
	}

	#[test]
	fn multiple_timescales_share_one_mapping() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();

		// Two sources, 90kHz video and 48kHz audio, anchored at the same arrival instant.
		let mut video = clock.source();
		let mut audio = clock.source();
		let v = video
			.translate_at(
				moq_net::Timestamp::new(180_000, moq_net::Timescale::new(90_000).unwrap()).unwrap(),
				1_000_000,
			)
			.unwrap();
		let a = audio
			.translate_at(
				moq_net::Timestamp::new(96_000, moq_net::Timescale::new(48_000).unwrap()).unwrap(),
				1_000_000,
			)
			.unwrap();

		// Both said "2s of content, live now": one broadcast instant, one wall time.
		assert_eq!(v.as_micros(), 1_000_000);
		assert_eq!(a.as_micros(), 1_000_000);
		assert_eq!(clock.wall_clock(v).unwrap(), clock.wall_clock(a).unwrap());

		// A media second later is a broadcast second later on both.
		let v2 = video
			.translate_at(
				moq_net::Timestamp::new(270_000, moq_net::Timescale::new(90_000).unwrap()).unwrap(),
				2_000_000,
			)
			.unwrap();
		let a2 = audio
			.translate_at(
				moq_net::Timestamp::new(144_000, moq_net::Timescale::new(48_000).unwrap()).unwrap(),
				2_000_000,
			)
			.unwrap();
		assert_eq!(v2.as_micros(), 2_000_000);
		assert_eq!(a2.as_micros(), 2_000_000);
	}

	#[test]
	fn reset_translation_preserves_the_idle_gap() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();

		assert_eq!(source.translate_at(us(0), 1_000_000).unwrap().as_micros(), 1_000_000);
		assert_eq!(
			source.translate_at(us(2_000_000), 3_000_000).unwrap().as_micros(),
			3_000_000
		);

		// The encoder restarts at zero 5s later: the broadcast continues after the gap, and the
		// wall epoch is untouched.
		let wall_before = clock.wall();
		let resumed = source.translate_at(us(0), 8_000_000).unwrap();
		assert_eq!(resumed.as_micros(), 8_000_000);
		assert_eq!(clock.wall(), wall_before);

		// Spacing resumes from the new anchor.
		let next = source.translate_at(us(1_000_000), 9_000_000).unwrap();
		assert_eq!(next.as_micros(), 9_000_000);
	}

	#[test]
	fn explicit_reset_marks_a_detected_restart() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();

		assert_eq!(source.translate_at(us(0), 1_000_000).unwrap().as_micros(), 1_000_000);
		// The adapter saw the restart out of band and re-anchors, even though the PTS did not
		// move backwards.
		let resumed = source.reset_at(us(0), 4_000_000).unwrap();
		assert_eq!(resumed.as_micros(), 4_000_000);
	}

	#[test]
	fn bframe_reordering_within_a_group_survives() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();

		assert_eq!(source.translate_at(us(0), 1_000_000).unwrap().as_micros(), 1_000_000);
		assert_eq!(
			source.translate_at(us(40_000), 1_040_000).unwrap().as_micros(),
			1_040_000
		);
		// A 20ms present-before-decode step back is reordering, not a reset: the offset stands.
		let reordered = source.translate_at(us(20_000), 1_040_000).unwrap();
		assert_eq!(reordered.as_micros(), 1_020_000);
		// And the broadcast continues from the reordered frontier.
		let next = source.translate_at(us(80_000), 1_080_000).unwrap();
		assert_eq!(next.as_micros(), 1_080_000);
	}

	#[test]
	fn mapping_past_u64_micros_is_refused() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();

		assert_eq!(source.translate_at(us(0), 0).unwrap().as_micros(), 0);
		// A seconds-scale timestamp whose microsecond mapping exceeds u64::MAX.
		let huge = moq_net::Timestamp::from_secs(u64::MAX / 1_000_000 + 2).unwrap();
		assert!(matches!(
			source.translate_at(huge, 0),
			Err(crate::Error::UnmappableTimestamp(_))
		));
	}

	#[test]
	fn mapping_before_the_broadcast_began_is_refused() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();

		// The first frame carries 100ms of PTS but arrives 50ms in: the offset is negative.
		assert_eq!(source.translate_at(us(100_000), 50_000).unwrap().as_micros(), 50_000);
		// A small step back stays within reorder tolerance but lands before broadcast zero.
		assert!(matches!(
			source.translate_at(us(0), 50_000),
			Err(crate::Error::UnmappableTimestamp(_))
		));
	}

	#[test]
	fn wall_mapping_survives_a_system_clock_adjustment() {
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
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
		let clock = Clock::new_at(epoch(), moq_epoch()).unwrap();
		// The largest representable broadcast timestamp still maps.
		let max = moq_net::Timestamp::from_micros((1u64 << 62) - 1).unwrap();
		assert!(clock.wall_clock(max).is_err(), "past the JSON-safe range is refused");
	}
}
