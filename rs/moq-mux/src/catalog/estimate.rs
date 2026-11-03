use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use moq_net::Timestamp;

/// The window over which bitrate and framerate are averaged before they are reported.
const RATE_WINDOW: Duration = Duration::from_secs(1);

/// The frame rates a measurement snaps to. Anything else (a variable-rate source's average)
/// publishes nothing, since consumers size clocks from the value (an fMP4 timescale of
/// `framerate * 1000`).
const FRAMERATES: [f64; 11] = [
	24_000.0 / 1001.0,
	24.0,
	25.0,
	30_000.0 / 1001.0,
	30.0,
	48.0,
	50.0,
	60_000.0 / 1001.0,
	60.0,
	100.0,
	120.0,
];

/// How far a measurement may sit from the rate it snaps to: half the 0.1% gap between an NTSC
/// rate and its integer neighbour, so no measurement is ambiguous between them.
const FRAMERATE_TOLERANCE: f64 = 0.0005;

/// The catalog fields an [`Estimator`] can measure from the frames fed to it.
///
/// An absent field is one to measure: whatever the config already carries when it reaches
/// [`container::Producer::set`](crate::container::Producer::set) is authoritative and left alone,
/// and the rest is detected and kept current.
#[derive(Clone, Default, Debug, PartialEq)]
#[non_exhaustive]
pub struct Estimate {
	/// The largest batch span or reorder delay written.
	pub jitter: Option<Duration>,
	/// The maximum bitrate in bits per second.
	pub bitrate: Option<u64>,
	/// The least a frame's timestamp has trailed the highest one another track had already
	/// written: the constant offset of a track that reaches the transport behind the others.
	/// Lateness only some frames have, such as B-frames written after a later reference picture,
	/// is variable and left to `jitter` and the receiver's own measurement.
	pub delay: Option<Duration>,
	/// The maximum frame rate in frames per second, when it is a standard one.
	pub framerate: Option<f64>,
}

impl Estimate {
	/// Set the jitter (or clear it with `None`).
	pub fn with_jitter(mut self, jitter: impl Into<Option<Duration>>) -> Self {
		self.jitter = jitter.into();
		self
	}

	/// Set the bitrate in bits per second (or clear it with `None`).
	pub fn with_bitrate(mut self, bitrate: impl Into<Option<u64>>) -> Self {
		self.bitrate = bitrate.into();
		self
	}

	/// Set the delay behind the other tracks (or clear it with `None`).
	pub fn with_delay(mut self, delay: impl Into<Option<Duration>>) -> Self {
		self.delay = delay.into();
		self
	}

	/// Set the frame rate in frames per second (or clear it with `None`).
	pub fn with_framerate(mut self, framerate: impl Into<Option<f64>>) -> Self {
		self.framerate = framerate.into();
		self
	}
}

/// Measures the catalog jitter, bitrate, delay, and framerate of one track from the frames written
/// to it, without reading a clock.
///
/// A [`container::Producer`](crate::container::Producer) created through the catalog owns one,
/// feeds it as you write, and publishes the result automatically:
///
/// ```no_run
/// # fn example<E: moq_mux::catalog::hang::CatalogExt>(
/// #     catalog: moq_mux::catalog::Producer<E>,
/// #     reserved: moq_mux::catalog::Reserved<E>,
/// #     net: moq_net::track::Producer,
/// #     config: hang::catalog::VideoConfig,
/// #     frame: moq_mux::container::Frame,
/// # ) -> moq_mux::Result<()> {
/// use moq_mux::catalog::hang::Container;
/// use moq_mux::container::Kind;
/// let mut track = reserved.video(net, Container::Legacy(Kind::Video), config)?;
/// track.write(frame)?;
/// # Ok(())
/// # }
/// ```
///
/// Drive one directly only when you write to a raw
/// [`track::Producer`](moq_net::track::Producer) instead. The methods mirror the ones on
/// [`container::Producer`](crate::container::Producer), so call them at the same points:
///
/// ```
/// # use moq_mux::catalog::Estimator;
/// # use moq_net::Timestamp;
/// let mut estimator = Estimator::new();
/// estimator.write(Timestamp::from_micros(0).unwrap(), 1200);
/// estimator.cut(Some(Timestamp::from_micros(33_000).unwrap()));
///
/// let estimate = estimator.estimate();
/// ```
///
/// A standalone estimator has no other tracks to order against, so it never measures a delay.
///
/// Move-only (not `Clone`): it owns the running measurement for exactly one track.
#[derive(Default)]
pub struct Estimator {
	jitter: Jitter,
	bitrate: Bitrate,
	framerate: Framerate,
	/// This track's place in the write order of every track sharing it.
	seat: Seat,
	/// The least a written timestamp has trailed the highest one another track had written, once
	/// any other track has written.
	delay: Option<Duration>,
}

impl Estimator {
	/// Create an empty estimator.
	pub fn new() -> Self {
		Self::default()
	}

	/// Measure `delay` against the other tracks sharing `order`.
	pub(crate) fn with_order(order: Order) -> Self {
		Self {
			seat: order.seat(),
			..Self::default()
		}
	}

	/// Observe a frame of `bytes` encoded bytes at presentation time `timestamp`, as written by
	/// [`container::Producer::write`](crate::container::Producer::write).
	pub fn write(&mut self, timestamp: Timestamp, bytes: usize) {
		self.rate(timestamp, bytes);
		self.order(timestamp);
	}

	/// Count a write toward the bitrate and framerate only.
	pub(crate) fn rate(&mut self, timestamp: Timestamp, bytes: usize) {
		let timestamp = nanos(timestamp);
		self.bitrate.write(timestamp, bytes);
		self.framerate.write(timestamp);
	}

	/// Place a frame stamped `timestamp` in the write order. It trails by how far the highest
	/// timestamp another track has already written is ahead of it: a frame the transport carries
	/// after later media from another track has to be waited for by a consumer playing both.
	///
	/// The delay is the least of those trails, so one frame that trails nothing clears it. Only a
	/// track every frame of which trails keeps one; the rest of the lateness varies from frame to
	/// frame, which the receiver measures and `jitter` floors.
	pub(crate) fn order(&mut self, timestamp: Timestamp) {
		if let Some(behind) = self.seat.write(nanos(timestamp)) {
			self.delay = Some(self.delay.map_or(behind, |delay| delay.min(behind)));
		}
	}

	/// Close the current span at `end`, as [`container::Producer::cut`](crate::container::Producer::cut)
	/// does for its group.
	///
	/// Bitrate is averaged over whole spans so a lone keyframe is never reported as the track
	/// bitrate on its own. A boundary with no usable duration (an unbounded cut, a span holding one
	/// frame) leaves the span open to fold into the next one, so nothing is lost by cutting often.
	pub fn cut(&mut self, end: Option<Timestamp>) {
		self.bitrate.cut(end.map(nanos));
		self.framerate.cut();
	}

	/// Discard the open bitrate span and this track's place in the write order, so nothing is
	/// measured across a break in the timeline. See
	/// [`container::Producer::discontinuity`](crate::container::Producer::discontinuity).
	///
	/// The track may resume at another offset from the others, so the least trail is measured
	/// afresh; a delay already in the catalog stays there.
	pub fn discontinuity(&mut self) {
		self.bitrate.discontinuity();
		self.framerate.discontinuity();
		self.seat.discontinuity();
		self.delay = None;
	}

	/// Observe a frame's reorder delay (`PTS - DTS`), which raises the jitter to the decode buffer a
	/// B-frame stream needs. Only a container knows this; the elementary stream carries no decode
	/// time.
	pub fn reorder(&mut self, delay: Timestamp) {
		self.burst(Duration::from(delay));
	}

	/// Record the media duration emitted together by a container importer.
	pub(crate) fn burst(&mut self, duration: Duration) {
		self.jitter.max = self.jitter.max.max(duration);
	}

	/// Everything measured so far, with what a catalog rendition needs to decide when to publish it.
	pub(crate) fn reading(&self) -> Reading {
		Reading {
			estimate: self.estimate(),
			period: self.framerate.shortest,
		}
	}

	/// Everything measured so far.
	///
	/// Catalog-owned [`container::Producer`](crate::container::Producer) handles publish this
	/// automatically when they cut or finish a group.
	pub fn estimate(&self) -> Estimate {
		Estimate {
			jitter: self.jitter.current(),
			bitrate: self.bitrate.current(),
			delay: self.delay.filter(|delay| !delay.is_zero()),
			framerate: self.framerate.max,
		}
	}
}

/// An [`Estimate`] with the measurements that pace its publishing.
#[derive(Clone, Debug, Default)]
pub(crate) struct Reading {
	pub estimate: Estimate,
	/// The shortest advance between frames, which a published `jitter` or `delay` rises in whole
	/// steps of when no framerate says the frame duration.
	pub period: Option<Duration>,
}

impl From<Estimate> for Reading {
	fn from(estimate: Estimate) -> Self {
		Self { estimate, period: None }
	}
}

/// A frame time as scale-free nanoseconds.
///
/// Timestamps reaching an estimator can carry different timescales: a track normalizes them into
/// its own on the way to the wire, but only there. `Timestamp` arithmetic refuses to mix scales, so
/// a span anchored on one scale could never be timed against a frame on another. Since a span now
/// survives a boundary it can't time, that would block the bitrate for the life of the track rather
/// than for one group. Normalizing on the way in is what makes the carry-forward safe.
fn nanos(timestamp: Timestamp) -> u128 {
	timestamp.as_nanos()
}

/// The time from `start` to `end`, or `None` if it didn't advance (a B-frame presenting earlier, a
/// span holding a single frame).
fn elapsed(start: u128, end: u128) -> Option<Duration> {
	let delta = end.checked_sub(start).filter(|delta| *delta > 0)?;
	// Saturating rather than wrapping. A frame gap over 584 years is nonsense either way, and the
	// clamp keeps this total instead of silently dropping the span.
	Some(Duration::from_nanos(u64::try_from(delta).unwrap_or(u64::MAX)))
}

/// Tracks the maximum bitrate in bits per second, averaged over whole spans.
///
/// A span's bytes are only counted once it is closed, and the average is taken over at least
/// [`RATE_WINDOW`] of media so a lone keyframe doesn't spike the reported value.
#[derive(Default)]
struct Bitrate {
	span: Option<Span>,
	window_bytes: u64,
	window_duration: Duration,
	max: Option<u64>,
}

impl Bitrate {
	fn write(&mut self, ts: u128, bytes: usize) {
		let span = self.span.get_or_insert(Span {
			start: ts,
			max: ts,
			bytes: 0,
		});

		span.start = span.start.min(ts);
		span.max = span.max.max(ts);
		span.bytes = span.bytes.saturating_add(bytes as u64);
	}

	fn cut(&mut self, end: Option<u128>) {
		let Some(span) = self.span.as_ref() else {
			return;
		};

		let duration = end
			.and_then(|end| elapsed(span.start, end))
			.or_else(|| elapsed(span.start, span.max));

		// Nothing to divide by yet. Leave the span open so its bytes join the next one rather than
		// vanishing: a track cut after every frame (the one-group-per-packet audio shape) reaches
		// this on every cut, and dropping the span there would leave the bitrate undetectable.
		let Some(duration) = duration else {
			return;
		};

		let span = self.span.take().expect("span is present");
		self.window_bytes = self.window_bytes.saturating_add(span.bytes);
		self.window_duration += duration;

		if self.window_duration < RATE_WINDOW {
			return;
		}

		let bitrate = bits_per_second(self.window_bytes, self.window_duration);
		self.window_bytes = 0;
		self.window_duration = Duration::ZERO;

		if self.max.is_none_or(|max| bitrate > max) {
			self.max = Some(bitrate);
		}
	}

	fn discontinuity(&mut self) {
		self.span = None;
	}

	fn current(&self) -> Option<u64> {
		self.max
	}
}

/// Tracks the maximum framerate: frames written per second of advance in the highest timestamp,
/// snapped to one of [`FRAMERATES`].
///
/// Not per span like [`Bitrate`]: a reordered group spans more than its own frames (an open GOP's
/// leading pictures present before its keyframe), while the highest timestamp advances one period
/// per frame whatever the order. Sampled at cuts, so every window starts and ends at the same point
/// of the group structure.
#[derive(Default)]
struct Framerate {
	/// The highest timestamp written, in scale-free nanoseconds.
	high: Option<u128>,
	/// The highest timestamp when the window opened.
	open: Option<u128>,
	/// Frames written since the window opened.
	frames: u64,
	max: Option<f64>,
	/// The shortest advance of the highest timestamp: a frame's duration, or a reordered group's
	/// span of them.
	shortest: Option<Duration>,
}

impl Framerate {
	fn write(&mut self, ts: u128) {
		if let Some(advance) = self.high.and_then(|high| elapsed(high, ts)) {
			self.shortest = Some(self.shortest.map_or(advance, |shortest| shortest.min(advance)));
		}
		self.high = Some(self.high.map_or(ts, |high| high.max(ts)));
		self.frames += 1;
	}

	fn cut(&mut self) {
		let Some(high) = self.high else {
			return;
		};
		// The first window opens at a cut too, not at the first frame, which may be a join that
		// starts partway into the group structure.
		let Some(open) = self.open else {
			self.open = Some(high);
			self.frames = 0;
			return;
		};
		let Some(duration) = elapsed(open, high).filter(|duration| *duration >= RATE_WINDOW) else {
			return;
		};
		let measured = self.frames as f64 / duration.as_secs_f64();
		let snapped = FRAMERATES
			.into_iter()
			.map(|rate| (rate, (measured - rate).abs() / rate))
			.filter(|(_, error)| *error <= FRAMERATE_TOLERANCE)
			.min_by(|a, b| a.1.total_cmp(&b.1))
			.map(|(rate, _)| rate);
		if let Some(framerate) = snapped
			&& self.max.is_none_or(|max| framerate > max)
		{
			self.max = Some(framerate);
		}
		self.open = Some(high);
		self.frames = 0;
	}

	/// Time nothing across a break in the timeline.
	fn discontinuity(&mut self) {
		*self = Self {
			max: self.max,
			shortest: self.shortest,
			..Self::default()
		};
	}
}

/// An open run of frames: everything written since the last boundary that could be timed.
struct Span {
	/// Scale-free nanoseconds, per [`nanos`].
	start: u128,
	max: u128,
	bytes: u64,
}

fn bits_per_second(bytes: u64, duration: Duration) -> u64 {
	let nanos = duration.as_nanos();
	if nanos == 0 {
		return 0;
	}

	let bits_per_second = (bytes as u128).saturating_mul(8).saturating_mul(1_000_000_000) / nanos;
	bits_per_second.min(u64::MAX as u128) as u64
}

/// The largest batch span or reorder delay. Once advertised it never falls.
#[derive(Default)]
struct Jitter {
	max: Duration,
}

impl Jitter {
	fn current(&self) -> Option<Duration> {
		(!self.max.is_zero()).then_some(self.max)
	}
}

/// The highest timestamp each track on one catalog has written, which every frame's `delay` is
/// measured against.
///
/// A frame is compared with the other tracks only: its own track's earlier frames say nothing
/// about the interleave, and a B-frame trailing its own reference picture is already reorder
/// jitter.
///
/// Every timestamp is a presentation time, written in decode order. A track with B-frames writes
/// each reference picture before the B-frames presenting ahead of it, so its high runs ahead of
/// what it writes, and frames of the other tracks written beside it trail that high too: another
/// rendition's B-frames, and the audio of the same decode slot. The reference pictures, and that
/// audio when it lines up with them, trail by nothing, which is why a track's `delay` is the
/// least it trails rather than the most. Only a constant offset between tracks survives, and that
/// is the one thing a receiver's measurement of each track against itself cannot see.
#[derive(Clone, Default)]
pub(crate) struct Order(Arc<Mutex<Highs>>);

impl Order {
	/// Take a place in the order for a new track.
	fn seat(&self) -> Seat {
		let index = self.0.lock().unwrap_or_else(PoisonError::into_inner).join();
		Seat {
			order: self.clone(),
			index,
		}
	}
}

/// Every seated track's highest timestamp, plus the two largest, so a write is constant time
/// however many tracks share the catalog.
#[derive(Default)]
struct Highs {
	/// Indexed by seat; `None` is a free seat, `Some(None)` a track that has written nothing since
	/// it joined or broke its timeline.
	seats: Vec<Option<Option<u128>>>,
	/// The largest high and its seat.
	first: Option<(usize, u128)>,
	/// The largest high on any other seat.
	second: Option<(usize, u128)>,
}

impl Highs {
	fn join(&mut self) -> usize {
		if let Some(index) = self.seats.iter().position(Option::is_none) {
			self.seats[index] = Some(None);
			return index;
		}
		self.seats.push(Some(None));
		self.seats.len() - 1
	}

	/// Raise `index`'s high to `timestamp` and return how far `timestamp` trails the others'
	/// highest, zero when it trails none, or `None` when no other track has a high to trail.
	fn write(&mut self, index: usize, timestamp: u128) -> Option<Duration> {
		let others = match self.first {
			Some((first, _)) if first == index => self.second,
			first => first,
		};

		let high = self.seats[index]
			.as_mut()
			.expect("a seated track")
			.get_or_insert(timestamp);
		*high = (*high).max(timestamp);
		let high = *high;

		// Highs only rise, so the top two stay ordered by updating them in place.
		match (self.first, self.second) {
			(Some((first, _)), _) if first == index => self.first = Some((index, high)),
			(Some((_, top)), _) if high > top => {
				self.second = self.first;
				self.first = Some((index, high));
			}
			(None, _) => self.first = Some((index, high)),
			(_, Some((second, low))) if second == index || high > low => self.second = Some((index, high)),
			(_, None) => self.second = Some((index, high)),
			_ => {}
		}

		let (_, others) = others?;
		Some(elapsed(timestamp, others).unwrap_or_default())
	}

	/// Forget `index`'s high, freeing its seat when `leave` is set.
	fn clear(&mut self, index: usize, leave: bool) {
		self.seats[index] = if leave { None } else { Some(None) };
		if [self.first, self.second]
			.iter()
			.flatten()
			.any(|(seat, _)| *seat == index)
		{
			self.rank();
		}
	}

	/// Recompute the top two from every seat.
	fn rank(&mut self) {
		self.first = None;
		self.second = None;
		for (index, seat) in self.seats.iter().enumerate() {
			let Some(high) = seat.flatten() else { continue };
			match self.first {
				Some((_, top)) if high <= top => {
					if self.second.is_none_or(|(_, low)| high > low) {
						self.second = Some((index, high));
					}
				}
				first => {
					self.second = first;
					self.first = Some((index, high));
				}
			}
		}
	}
}

/// One track's place in an [`Order`], released when the track's estimator drops.
struct Seat {
	order: Order,
	index: usize,
}

impl Default for Seat {
	fn default() -> Self {
		Order::default().seat()
	}
}

impl Seat {
	fn write(&self, timestamp: u128) -> Option<Duration> {
		self.lock().write(self.index, timestamp)
	}

	/// A break in the timeline may resume anywhere, so the old high must not hold others back.
	fn discontinuity(&self) {
		self.lock().clear(self.index, false);
	}

	fn lock(&self) -> std::sync::MutexGuard<'_, Highs> {
		self.order.0.lock().unwrap_or_else(PoisonError::into_inner)
	}
}

impl Drop for Seat {
	fn drop(&mut self) {
		self.lock().clear(self.index, true);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn micros(value: u64) -> Timestamp {
		Timestamp::from_micros(value).unwrap()
	}

	#[test]
	fn decode_order_pts_gap_never_becomes_provisional_jitter() {
		let mut estimator = Estimator::new();
		for pts in [0, 120_000, 40_000, 80_000] {
			estimator.write(micros(pts), 1);
		}
		assert_eq!(estimator.estimate().jitter, None);
		estimator.reorder(micros(80_000));
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(80)));
	}

	/// Estimators sharing one order, as the tracks of one catalog do.
	fn tracks<const N: usize>() -> [Estimator; N] {
		let order = Order::default();
		std::array::from_fn(|_| Estimator::with_order(order.clone()))
	}

	fn ms(value: u64) -> Duration {
		Duration::from_millis(value)
	}

	/// Video at 20 ms and audio at 15 ms, written `v0 v20 a0 a15 v40 a30 v60 a45 a60`: audio
	/// trails video by anything from nothing to 20 ms, which varies frame to frame and so is no
	/// constant offset.
	#[test]
	fn an_interleave_is_not_delay() {
		let [mut video, mut audio] = tracks();
		for (video_frame, pts) in [
			(true, 0),
			(true, 20),
			(false, 0),
			(false, 15),
			(true, 40),
			(false, 30),
			(true, 60),
			(false, 45),
			(false, 60),
		] {
			let track = if video_frame { &mut video } else { &mut audio };
			track.write(micros(pts * 1_000), 1);
		}
		assert_eq!(audio.estimate().delay, None);
		assert_eq!(video.estimate().delay, None);
	}

	/// A track every frame of which trails keeps the least of those trails: the constant offset,
	/// without the lateness only some of its frames add.
	#[test]
	fn a_constant_offset_is_delay() {
		let [mut video, mut audio] = tracks();
		for i in 0..50u64 {
			video.write(micros(100_000 + i * 20_000), 1);
			// Every third frame trails a further 10 ms on top of the 100 ms offset.
			let late = if i % 3 == 1 { 10_000 } else { 0 };
			audio.write(micros(i * 20_000 - late), 1);
		}
		assert_eq!(audio.estimate().delay, Some(ms(100)));
		assert_eq!(video.estimate().delay, None);
	}

	/// Two renditions of one 50 fps source with two B-frames (I0 P60 B20 B40 P120 B80 B100 …) and
	/// audio, written in lockstep in decode order: one frame of each track per 20 ms.
	///
	/// From [moq-dev/moq#7579](https://github.com/moq-dev/moq/issues/7579).
	fn lockstep(declare_reorder: bool) -> [Estimate; 3] {
		let [mut hevc, mut avc, mut audio] = tracks();
		if declare_reorder {
			// What the MPEG-TS and fMP4 importers report for this GOP: its largest PTS - DTS, with
			// decoding starting one frame before the first PTS.
			hevc.reorder(micros(60_000));
			avc.reorder(micros(60_000));
		}
		let decode_order = [0, 60, 20, 40].into_iter().chain((1..10u64).flat_map(|gop| {
			let base = gop * 60;
			[base + 60, base + 20, base + 40]
		}));
		for (tick, pts) in decode_order.enumerate() {
			hevc.write(micros(pts * 1_000), 1);
			avc.write(micros(pts * 1_000), 1);
			audio.write(micros(tick as u64 * 20_000), 1);
		}
		[hevc.estimate(), avc.estimate(), audio.estimate()]
	}

	/// B-frame reorder is never `delay`: not on the B-frame tracks, and not on the audio beside
	/// them. Only some frames trail, by the reorder, which is variable lateness the receiver
	/// already measures; counting it as `delay` too would buffer it twice.
	#[test]
	fn bframe_reorder_is_not_delay() {
		let [hevc, avc, audio] = lockstep(false);
		assert_eq!((hevc.jitter, hevc.delay), (None, None));
		assert_eq!((avc.jitter, avc.delay), (None, None));
		assert_eq!((audio.jitter, audio.delay), (None, None));

		// A declared reorder is the video's jitter, and still nobody's delay.
		let [hevc, avc, audio] = lockstep(true);
		assert_eq!((hevc.jitter, hevc.delay), (Some(ms(60)), None));
		assert_eq!((avc.jitter, avc.delay), (Some(ms(60)), None));
		assert_eq!((audio.jitter, audio.delay), (None, None));
	}

	#[test]
	fn a_lone_track_never_measures_a_delay() {
		let [mut video] = tracks();
		// Decode order with B-frames: each trails a reference picture written before it.
		for pts in [0, 120_000, 40_000, 80_000] {
			video.write(micros(pts), 1);
		}
		assert_eq!(video.estimate(), Estimate::default());
	}

	/// A B-frame trailing its own track is reorder jitter, not a delay behind the other tracks.
	#[test]
	fn reorder_within_a_track_is_not_delay() {
		let [mut video, mut audio] = tracks();
		audio.write(micros(0), 1);
		for pts in [0, 120_000, 40_000, 80_000] {
			video.write(micros(pts), 1);
		}
		assert_eq!(video.estimate().delay, None);
	}

	/// A track that breaks its timeline may resume behind where it was, so its old high no longer
	/// holds the others back.
	#[test]
	fn a_discontinuity_drops_the_tracks_high() {
		let [mut ahead, mut steady] = tracks();
		ahead.write(micros(10_000_000), 1);
		ahead.discontinuity();
		ahead.write(micros(1_000_000), 1);
		steady.write(micros(1_000_000), 1);
		assert_eq!(steady.estimate().delay, None);
	}

	/// A track that resumes behind where it was trails by its new offset, however little it
	/// trailed before the break.
	#[test]
	fn a_discontinuity_measures_the_new_offset() {
		let [mut video, mut audio] = tracks();
		video.write(micros(0), 1);
		audio.write(micros(0), 1);
		audio.discontinuity();
		for i in 1..10u64 {
			video.write(micros(1_000_000 + i * 20_000), 1);
			audio.write(micros(800_000 + i * 20_000), 1);
		}
		assert_eq!(audio.estimate().delay, Some(ms(200)));
	}

	#[test]
	fn a_dropped_track_stops_holding_others_back() {
		let order = Order::default();
		let mut ahead = Estimator::with_order(order.clone());
		ahead.write(micros(10_000_000), 1);
		drop(ahead);

		let mut steady = Estimator::with_order(order.clone());
		let mut other = Estimator::with_order(order);
		other.write(micros(1_000_000), 1);
		steady.write(micros(1_000_000), 1);
		assert_eq!(steady.estimate().delay, None);
	}

	/// The top two kept incrementally agree with a scan over every seat, through joins, leaves,
	/// and breaks.
	#[test]
	fn highs_match_a_full_scan() {
		let mut highs = Highs::default();
		let mut live: Vec<usize> = Vec::new();
		let mut state = 0x9e37_79b9_7f4a_7c15u64;
		let mut next = |bound: u64| {
			state ^= state << 13;
			state ^= state >> 7;
			state ^= state << 17;
			state % bound
		};

		for step in 0..10_000u64 {
			match next(10) {
				0 if live.len() < 6 => live.push(highs.join()),
				1 if !live.is_empty() => {
					let index = live.swap_remove(next(live.len() as u64) as usize);
					highs.clear(index, true);
				}
				2 if !live.is_empty() => highs.clear(live[next(live.len() as u64) as usize], false),
				_ if !live.is_empty() => {
					let index = live[next(live.len() as u64) as usize];
					let timestamp = (step + next(500)) as u128;
					let others = live
						.iter()
						.filter(|seat| **seat != index)
						.filter_map(|seat| highs.seats[*seat].flatten())
						.max();
					let expected = others.map(|others| elapsed(timestamp, others).unwrap_or_default());
					assert_eq!(highs.write(index, timestamp), expected, "step {step}");
				}
				_ => live.push(highs.join()),
			}
		}
	}

	#[test]
	fn bitrate_waits_for_the_window_and_reports_the_maximum() {
		let mut estimator = Estimator::new();

		estimator.write(micros(0), 100_000);
		estimator.write(micros(500_000), 100_000);
		estimator.cut(Some(micros(1_000_000)));
		assert_eq!(estimator.estimate().bitrate, Some(1_600_000));

		// A quieter second of media doesn't lower the maximum.
		estimator.write(micros(1_000_000), 25_000);
		estimator.cut(Some(micros(2_000_000)));
		assert_eq!(estimator.estimate().bitrate, Some(1_600_000));

		estimator.write(micros(2_000_000), 250_000);
		estimator.cut(Some(micros(3_000_000)));
		assert_eq!(estimator.estimate().bitrate, Some(2_000_000));
	}

	/// A span that can't be timed stays open instead of being dropped, so a track cut after every
	/// frame (one group per packet, how the importer facade drives audio) still measures a bitrate.
	/// The unbounded cut is a no-op and the next frame's timestamp closes the span exactly.
	#[test]
	fn unbounded_cuts_never_drop_bytes() {
		let mut estimator = Estimator::new();

		// 40 packets of 5 kB at 40ms spacing: 1 Mbps, one group per packet.
		for i in 0..40u64 {
			let ts = micros(i * 40_000);
			// What `container::Producer::write` does for the keyframe opening each group...
			estimator.cut(Some(ts));
			estimator.write(ts, 5_000);
			// ...and what the facade's per-frame close does right after.
			estimator.cut(None);
		}

		assert_eq!(estimator.estimate().bitrate, Some(1_000_000));
	}

	/// A track whose timestamps change scale mid-stream still measures. `Timestamp` arithmetic
	/// refuses to mix scales, and since a span now survives a boundary it can't time, an unnormalized
	/// anchor would block the bitrate for the life of the track instead of for one group.
	#[test]
	fn mixed_timescales_still_measure() {
		let mut estimator = Estimator::new();

		// The opening frame is millisecond-scale (a clock stamp standing in for a missing PTS); the
		// rest are microsecond-scale, as a parsed elementary stream would be.
		estimator.write(Timestamp::from_millis(0).unwrap(), 5_000);
		for i in 1..40u64 {
			let ts = micros(i * 40_000);
			estimator.cut(Some(ts));
			estimator.write(ts, 5_000);
		}

		let estimate = estimator.estimate();
		assert_eq!(estimate.bitrate, Some(1_000_000));
		assert_eq!(estimate.jitter, None);
	}

	/// Write groups `groups` of `order.len()` frames at 25 fps, in decode order with `order` giving
	/// each frame's display offset from its keyframe, cutting at every keyframe like
	/// `container::Producer::write`. Group 0 starts a second in, so leading pictures stay positive.
	fn feed_groups(estimator: &mut Estimator, groups: std::ops::Range<u64>, order: &[i64]) {
		for group in groups {
			let keyframe = 25 + (group * order.len() as u64) as i64;
			for (i, &offset) in order.iter().enumerate() {
				let ts = micros(((keyframe + offset) as u64) * 40_000);
				if i == 0 {
					estimator.cut(Some(ts));
				}
				estimator.write(ts, 1_000);
			}
		}
	}

	#[test]
	fn framerate_waits_for_the_window() {
		let mut estimator = Estimator::new();
		// Half a second of 25 fps.
		let order: Vec<i64> = (0..12).collect();
		feed_groups(&mut estimator, 0..1, &order);
		estimator.cut(None);
		assert_eq!(estimator.estimate().framerate, None);

		feed_groups(&mut estimator, 1..5, &order);
		assert_eq!(estimator.estimate().framerate, Some(25.0));
	}

	/// B-frames in decode order: every group presents a frame below the one written before it.
	#[test]
	fn framerate_counts_reordered_groups() {
		let mut estimator = Estimator::new();
		// IPBB in a 24-frame closed GOP.
		let mut order = vec![0];
		for quad in 0..(23 / 3) {
			let base = quad * 3;
			order.extend([base + 3, base + 1, base + 2]);
		}
		order.extend(22..24);
		assert_eq!(order.len(), 24);
		feed_groups(&mut estimator, 0..5, &order);
		assert_eq!(estimator.estimate().framerate, Some(25.0));
	}

	/// An open GOP's leading pictures present before their keyframe, so a group spans more than
	/// its own frames: timing groups by their span would read 24 frames over 27 periods.
	#[test]
	fn framerate_counts_leading_pictures() {
		let mut estimator = Estimator::new();
		// Keyframe, then three leading B-frames presented before it, then the rest.
		let mut order = vec![0, -3, -2, -1];
		order.extend(1..21);
		assert_eq!(order.len(), 24);
		feed_groups(&mut estimator, 0..5, &order);
		assert_eq!(estimator.estimate().framerate, Some(25.0));
	}

	/// 30000/1001 fps on a 90 kHz clock: no period is a whole number of nanoseconds, and the
	/// measurement is 0.1% from 30 fps.
	#[test]
	fn framerate_snaps_to_ntsc() {
		let mut estimator = Estimator::new();
		for i in 0..300u64 {
			let ts = Timestamp::from_scale(i * 3003, 90_000).unwrap();
			estimator.cut(Some(ts));
			estimator.write(ts, 1_000);
			estimator.cut(None);
		}
		assert_eq!(estimator.estimate().framerate, Some(30_000.0 / 1001.0));
	}

	/// A variable-rate source whose windows average no standard rate publishes nothing.
	#[test]
	fn framerate_skips_nonstandard_rates() {
		let mut estimator = Estimator::new();
		// 27 fps: 37 ms apart, give or take.
		for i in 0..100u64 {
			let ts = micros(i * 37_037);
			estimator.cut(Some(ts));
			estimator.write(ts, 1_000);
		}
		assert_eq!(estimator.estimate().framerate, None);
	}

	/// The highest rate over any window is reported, and a slower one doesn't lower it.
	#[test]
	fn framerate_reports_the_maximum() {
		let mut estimator = Estimator::new();
		let order: Vec<i64> = (0..25).collect();
		feed_groups(&mut estimator, 0..3, &order);
		assert_eq!(estimator.estimate().framerate, Some(25.0));

		for i in 0..40u64 {
			let ts = micros(4_000_000 + i * 41_667);
			estimator.cut(Some(ts));
			estimator.write(ts, 1_000);
		}
		assert_eq!(estimator.estimate().framerate, Some(25.0), "24 fps doesn't lower it");
	}

	/// A break in the timeline discards the open span rather than timing it across the gap, and
	/// resets the frame spacing so the gap is never mistaken for a frame duration.
	#[test]
	fn discontinuity_drops_the_open_span() {
		let mut estimator = Estimator::new();

		estimator.write(micros(0), 100_000);
		estimator.discontinuity();

		// Resumed 40 minutes later. Without the reset this would time 100 kB across the gap.
		estimator.write(micros(2_400_000_000), 100_000);
		estimator.cut(Some(micros(2_401_000_000)));

		assert_eq!(
			estimator.estimate().bitrate,
			Some(800_000),
			"only the post-break span counts"
		);
		assert_eq!(estimator.estimate().jitter, None, "the gap is not a frame duration");
	}

	/// Frames on either side of a break are never timed across it.
	#[test]
	fn discontinuity_resets_the_framerate_window() {
		let mut estimator = Estimator::new();
		for i in 0..10u64 {
			estimator.write(micros(i * 40_000), 1_000);
		}
		estimator.discontinuity();

		// Resumed 40 minutes later at 25 fps. Timed across the gap it would read ~0 fps.
		for i in 0..60u64 {
			let ts = micros(2_400_000_000 + i * 40_000);
			estimator.cut(Some(ts));
			estimator.write(ts, 1_000);
		}
		assert_eq!(estimator.estimate().framerate, Some(25.0));
	}
}
