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
/// Copies share the epoch and the fixed wall mapping, so handing them to several producers keeps
/// the broadcast on one clock. The wall mapping is established at construction: sampling it when
/// a delayed first frame arrives would pretend that frame is timestamp zero, so the epoch is
/// pinned up front and that frame's PTS converts into the mapping instead.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
	epoch: Instant,
	wall: hang::catalog::Clock,
}

impl Clock {
	/// Units per second for the broadcast clock: microseconds.
	///
	/// Matches the hang container timescale, so media timestamps convert without loss, and fine
	/// enough that the wall value stays within the JSON-safe integer range until the year 2255.
	pub const TIMESCALE: moq_net::Timescale = moq_net::Timescale::MICRO;

	/// How far before construction a fresh clock puts PTS zero.
	///
	/// A translated source anchors its first frame at the current instant, but the frames
	/// muxed beside it can carry earlier timestamps: a B-frame presenting before the keyframe
	/// decoded ahead of it, or audio leading video in the mux. Starting the clock this far back
	/// leaves them room instead of landing before the broadcast began.
	const LEAD: Duration = Duration::from_secs(10);

	/// Start a clock at the current instant, with PTS zero ten seconds earlier on both the
	/// monotonic and the wall clock, so earlier-stamped frames of a source anchored now still map.
	pub fn new() -> Self {
		let (now, wall) = (Instant::now(), SystemTime::now());
		// Shortly after boot the monotonic clock may not reach back that far; start at now then.
		let (epoch, wall) = match (now.checked_sub(Self::LEAD), wall.checked_sub(Self::LEAD)) {
			(Some(epoch), Some(wall)) => (epoch, wall),
			_ => (now, wall),
		};
		Self::at(epoch, wall).expect("the current wall time is representable as a broadcast clock")
	}

	/// Start a clock at an explicit monotonic epoch and wall time.
	///
	/// The deterministic constructor: synthetic sources and fixtures pin both ends instead of
	/// sampling. Refuses an unrepresentable wall.
	pub fn at(epoch: Instant, wall: SystemTime) -> crate::Result<Self> {
		Ok(Self {
			epoch,
			wall: wall_clock(wall)?,
		})
	}

	/// The current timestamp since the clock's epoch.
	pub fn now(&self) -> moq_net::Timestamp {
		// u128 -> u64 truncation is unreachable: u64 microseconds is ~584,000 years.
		moq_net::Timestamp::from_micros(self.epoch.elapsed().as_micros() as u64)
			.expect("an instant elapsed duration fits in a timestamp")
	}

	/// Map the instant a payload was captured (a datagram's arrival, a sensor read) onto this clock.
	///
	/// Refuses an instant ahead of now, which would claim the payload reached the transport before
	/// it existed, and one before the clock's epoch, which no timestamp can name.
	pub(crate) fn capture(&self, at: Instant) -> crate::Result<moq_net::Timestamp> {
		if at > Instant::now() {
			return Err(crate::Error::InvalidCapture);
		}
		let elapsed = at
			.checked_duration_since(self.epoch)
			.ok_or(crate::Error::InvalidCapture)?;
		Ok(moq_net::Timestamp::from_micros(elapsed.as_micros() as u64)
			.expect("an instant elapsed duration fits in a timestamp"))
	}

	/// Map a payload's capture instant onto this clock, keeping the payload.
	pub(crate) fn stamp<P>(&self, timed: moq_net::Timed<P, Instant>) -> crate::Result<moq_net::Timed<P>> {
		let at = timed.at.map(|at| self.capture(at)).transpose()?;
		Ok(moq_net::Timed { value: timed.value, at })
	}

	/// Units per second for [`wall`](Self::wall): [`TIMESCALE`](Self::TIMESCALE).
	pub fn timescale(&self) -> moq_net::Timescale {
		Self::TIMESCALE
	}

	/// The catalog root section advertising this clock.
	pub fn wall(&self) -> hang::catalog::Clock {
		self.wall
	}

	/// The wall-clock time of `pts` under this broadcast's fixed mapping.
	///
	/// Pure in the stored epoch: a system-clock adjustment after construction changes nothing.
	/// Refuses an unrepresentable result rather than truncating it.
	pub fn wall_clock(&self, pts: moq_net::Timestamp) -> crate::Result<SystemTime> {
		self.wall.wall_clock(pts).map_err(crate::Error::from)
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
/// inside a group survives verbatim. Translated timestamps keep the source's timescale.
///
/// The broadcast wall mapping is never touched: translating a reset is not a new epoch, and a
/// discontinuity marker the adapter emits alongside is a delivery event the playhead reacts to,
/// not a clock the catalog republishes. Retained records keep their timestamps.
///
/// Each publisher adapter owns one per source and wires its own restart detection to
/// [`reset`](Self::reset); the automatic path only separates reordering from resets by size.
pub struct SourceMap {
	anchor: Anchor,
	lane: Lane,
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
			anchor: Anchor::new(clock),
			lane: Lane::default(),
		}
	}

	/// The broadcast clock this source translates onto.
	pub fn clock(&self) -> Clock {
		self.anchor.clock
	}

	/// Translate `pts` onto the broadcast clock, sampling the arrival time.
	pub fn translate(&mut self, pts: moq_net::Timestamp) -> crate::Result<moq_net::Timestamp> {
		self.anchor.translate(&mut self.lane, pts)
	}

	/// Translate `pts` onto the broadcast clock, arriving at monotonic `now` micros.
	///
	/// The deterministic core behind [`translate`](Self::translate): synthetic sources pin the
	/// arrival instants instead of sampling them.
	pub fn translate_at(&mut self, pts: moq_net::Timestamp, now: u64) -> crate::Result<moq_net::Timestamp> {
		self.anchor.translate_at(&mut self.lane, pts, now)
	}

	/// Re-anchor after an explicitly detected source restart, preserving the idle gap.
	///
	/// The adapter path for a restart it observes out of band (an encoder reload, a file loop):
	/// the next frame continues after everything published so far plus the downtime since the
	/// previous frame, instead of rewinding the broadcast.
	pub fn reset(&mut self, pts: moq_net::Timestamp) -> crate::Result<moq_net::Timestamp> {
		self.lane.restart();
		self.translate(pts)
	}

	/// [`reset`](Self::reset) with an explicit arrival instant, for synthetic sources.
	pub fn reset_at(&mut self, pts: moq_net::Timestamp, now: u64) -> crate::Result<moq_net::Timestamp> {
		self.lane.restart();
		self.translate_at(pts, now)
	}
}

/// One source's mapping onto the broadcast clock, shared by every track the source muxes.
///
/// Tracks of one source must share an offset or they drift apart by however far their first
/// frames' PTS differ. They can't share a single [`SourceMap`] either: interleaved audio and
/// video step back further than [`SourceMap::MAX_REORDER`], which would read as a reset. So
/// each track keeps its own [`Lane`] that detects its own backwards steps, and a restart any
/// lane detects moves the anchor once; the other lanes adopt that mapping when they restart too.
pub(crate) struct Anchor {
	clock: Clock,
	/// Broadcast micros minus source micros for the current generation; `None` until the first
	/// frame anchors it.
	offset: Option<i128>,
	/// Bumped at each re-anchor, so a lane knows whether its restart was already applied.
	generation: u64,
	/// The latest broadcast micros published so far, by any lane: the idle gap's origin.
	last_broadcast: Option<u128>,
	/// Where the latest frames end, by any lane, so a restart never lands on one of them.
	last_end: Option<u128>,
	/// `clock.now()` when the last frame was translated: the idle gap's start.
	last_arrival: Option<u64>,
}

/// One track's position on its source's [`Anchor`].
#[derive(Default)]
pub(crate) struct Lane {
	/// The offset this lane translates with, in micros; `None` until it adopts one.
	offset: Option<i128>,
	generation: u64,
	last_source: Option<u128>,
	/// The shortest forward step this lane's source took: its frame duration, near enough.
	step: Option<u128>,
	/// The adapter observed a restart on this lane out of band.
	restart: bool,
}

impl Lane {
	/// The next frame starts a new source timeline, however its PTS compares to the last.
	pub(crate) fn restart(&mut self) {
		self.restart = true;
	}
}

impl Anchor {
	pub(crate) fn new(clock: Clock) -> Self {
		Self {
			clock,
			offset: None,
			generation: 0,
			last_broadcast: None,
			last_end: None,
			last_arrival: None,
		}
	}

	/// Translate one of `lane`'s timestamps, sampling the arrival time.
	pub(crate) fn translate(&mut self, lane: &mut Lane, pts: moq_net::Timestamp) -> crate::Result<moq_net::Timestamp> {
		self.translate_at(lane, pts, self.clock.now().value())
	}

	/// Translate one of `lane`'s timestamps, arriving at monotonic `now` micros.
	pub(crate) fn translate_at(
		&mut self,
		lane: &mut Lane,
		pts: moq_net::Timestamp,
		now: u64,
	) -> crate::Result<moq_net::Timestamp> {
		let src = pts.as_micros();

		match self.offset {
			// The first frame is live now; the source keeps its spacing from there. Rebasing by
			// the frame's own PTS (rather than pretending it is timestamp zero) is what keeps a
			// delayed first frame honest.
			None => self.offset = Some(now as i128 - src as i128),
			Some(_) => {
				let stepped_back = lane
					.last_source
					.is_some_and(|last| last > src + SourceMap::MAX_REORDER.as_micros());
				// A restart another lane already applied is adopted, not applied twice.
				if (lane.restart || stepped_back) && lane.offset.is_some() && lane.generation == self.generation {
					self.reanchor(src, now);
				}
				if lane.restart || stepped_back {
					lane.offset = None;
				}
			}
		}
		lane.restart = false;

		// A lane joining late, or following a restart, takes the source's current mapping.
		let offset = *lane.offset.get_or_insert_with(|| {
			lane.generation = self.generation;
			self.offset.expect("anchored above")
		});

		let mapped = src as i128 + offset;
		if mapped < 0 {
			return Err(crate::Error::UnmappableTimestamp(format!(
				"{pts:?} lands before the broadcast began"
			)));
		}
		// The broadcast clock counts in micros, so the mapping must be nameable there too.
		u64::try_from(mapped)
			.ok()
			.and_then(|mapped| moq_net::Timestamp::from_micros(mapped).ok())
			.ok_or_else(|| {
				crate::Error::UnmappableTimestamp(format!("{pts:?} lands outside the representable range"))
			})?;

		// Keep the source's timescale: the offset is constant, so the spacing stays exact.
		let scale = pts.scale();
		let shift = offset * scale.as_u64() as i128 / 1_000_000;
		let value = u64::try_from(pts.value() as i128 + shift)
			.map_err(|_| crate::Error::UnmappableTimestamp(format!("{pts:?} lands outside the representable range")))?;
		let translated = moq_net::Timestamp::new(value, scale)
			.map_err(|_| crate::Error::UnmappableTimestamp(format!("{pts:?} lands outside the representable range")))?;

		if let Some(step) = lane
			.last_source
			.and_then(|last| src.checked_sub(last))
			.filter(|step| *step > 0)
		{
			lane.step = Some(lane.step.map_or(step, |min| min.min(step)));
		}
		lane.last_source = Some(src);

		let start = mapped as u128;
		self.last_broadcast = Some(self.last_broadcast.map_or(start, |last| last.max(start)));
		self.extend_micros(start + lane.step.unwrap_or(0));
		self.last_arrival = Some(now);
		Ok(translated)
	}

	/// Record that the broadcast has published up to `end`, e.g. a fragment's last sample end.
	pub(crate) fn extend(&mut self, end: moq_net::Timestamp) {
		self.extend_micros(end.as_micros());
	}

	fn extend_micros(&mut self, end: u128) {
		self.last_end = Some(self.last_end.map_or(end, |last| last.max(end)));
	}

	/// Move the anchor so `src` continues after everything published plus the idle gap since the
	/// previous arrival: the real downtime for a paced source, and at least the last frames' end
	/// for one arriving in a burst.
	fn reanchor(&mut self, src: u128, now: u64) {
		let base = match (self.last_broadcast, self.last_arrival) {
			(Some(last), Some(arrival)) => {
				let idle = last + now.saturating_sub(arrival) as u128;
				idle.max(self.last_end.unwrap_or(0))
			}
			_ => now as u128,
		};
		self.offset = Some(base as i128 - src as i128);
		self.generation += 1;
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
		assert_eq!(clock.epoch, shared.epoch);
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
	fn delayed_first_frame_anchors_live() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
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
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();

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
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
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
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();

		assert_eq!(source.translate_at(us(0), 1_000_000).unwrap().as_micros(), 1_000_000);
		// The adapter saw the restart out of band and re-anchors, even though the PTS did not
		// move backwards.
		let resumed = source.reset_at(us(0), 4_000_000).unwrap();
		assert_eq!(resumed.as_micros(), 4_000_000);
	}

	#[test]
	fn bframe_reordering_within_a_group_survives() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
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
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
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

	#[test]
	fn muxed_lanes_share_one_mapping() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		let mut anchor = Anchor::new(clock);
		let (mut video, mut audio) = (Lane::default(), Lane::default());

		// Video anchors live; audio, muxed 800ms earlier, joins on the same offset.
		let v = anchor.translate_at(&mut video, us(10_800_000), 2_000_000).unwrap();
		assert_eq!(v.as_micros(), 2_000_000);
		let a = anchor.translate_at(&mut audio, us(10_000_000), 2_000_000).unwrap();
		assert_eq!(a.as_micros(), 1_200_000);

		// Interleaving steps back further than a reorder across lanes, which is not a reset.
		let v = anchor.translate_at(&mut video, us(11_800_000), 3_000_000).unwrap();
		assert_eq!(v.as_micros(), 3_000_000);
		let a = anchor.translate_at(&mut audio, us(11_000_000), 3_000_000).unwrap();
		assert_eq!(a.as_micros(), 2_200_000);
	}

	#[test]
	fn muxed_restart_reanchors_once() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		let mut anchor = Anchor::new(clock);
		let (mut video, mut audio) = (Lane::default(), Lane::default());

		anchor.translate_at(&mut video, us(5_000_000), 1_000_000).unwrap();
		anchor.translate_at(&mut audio, us(5_000_000), 1_000_000).unwrap();

		// The source restarts at zero after 4s idle: video notices first and moves the anchor to
		// the last published instant plus the gap.
		let v = anchor.translate_at(&mut video, us(0), 5_000_000).unwrap();
		assert_eq!(v.as_micros(), 5_000_000);
		// Audio's own step back adopts that mapping rather than adding the gap again.
		let a = anchor.translate_at(&mut audio, us(20_000), 5_020_000).unwrap();
		assert_eq!(a.as_micros(), 5_020_000);

		// An out-of-band restart flagged on every lane is applied once as well.
		video.restart();
		audio.restart();
		let v = anchor.translate_at(&mut video, us(0), 6_000_000).unwrap();
		assert_eq!(v.as_micros(), 6_000_000);
		let a = anchor.translate_at(&mut audio, us(0), 6_000_000).unwrap();
		assert_eq!(a.as_micros(), 6_000_000);
	}

	#[test]
	fn translation_keeps_the_source_timescale() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
		let mut source = clock.source();
		let scale = moq_net::Timescale::new(90_000).unwrap();

		let first = source
			.translate_at(moq_net::Timestamp::new(3003, scale).unwrap(), 1_000_000)
			.unwrap();
		let second = source
			.translate_at(moq_net::Timestamp::new(6006, scale).unwrap(), 1_033_000)
			.unwrap();
		// 90 kHz in, 90 kHz out, with the frame spacing exact in ticks.
		assert_eq!(first.scale(), scale);
		assert_eq!(second.value() - first.value(), 3003);
	}

	#[test]
	fn mapping_before_the_broadcast_began_is_refused() {
		let clock = Clock::at(epoch(), moq_epoch()).unwrap();
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
