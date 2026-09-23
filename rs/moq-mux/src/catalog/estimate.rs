use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_net::Timestamp;

/// The window over which bitrate is averaged before it is reported.
const BITRATE_WINDOW: Duration = Duration::from_secs(1);
const JITTER_WINDOW: Duration = Duration::from_secs(10);

/// The catalog fields an [`Estimator`] can measure from the frames fed to it.
///
/// An absent field is one to measure: whatever the config already carries when it reaches
/// [`container::Producer::set`](crate::container::Producer::set) is authoritative and left alone,
/// and the rest is detected and kept current.
#[derive(Clone, Default, Debug, PartialEq)]
#[non_exhaustive]
pub struct Estimate {
	/// The maximum delay between a frame being ready and the publisher flushing it.
	pub jitter: Option<Duration>,
	/// The maximum bitrate in bits per second.
	pub bitrate: Option<u64>,
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
}

/// Measures the catalog jitter and bitrate of one track from the frames written to it.
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
/// Move-only (not `Clone`): it owns the running measurement for exactly one track.
#[derive(Default)]
pub struct Estimator {
	jitter: Jitter,
	bitrate: Bitrate,
	baseline: Arc<Mutex<Baseline>>,
}

impl Estimator {
	/// Create an empty estimator.
	pub fn new() -> Self {
		Self::default()
	}

	pub(crate) fn with_baseline(baseline: Arc<Mutex<Baseline>>) -> Self {
		Self { baseline, ..Self::default() }
	}

	/// Observe a frame of `bytes` encoded bytes at presentation time `timestamp`, as written by
	/// [`container::Producer::write`](crate::container::Producer::write).
	pub fn write(&mut self, timestamp: Timestamp, bytes: usize) {
		let timestamp = nanos(timestamp);
		self.bitrate.write(timestamp, bytes);
	}

	fn observe_flush_at(&mut self, timestamp: u128, now: u128) {
		let spread = self.baseline.lock().unwrap().observe(timestamp, now);
		self.jitter.max = self.jitter.max.max(spread);
	}

	/// Close the current span at `end`, as [`container::Producer::cut`](crate::container::Producer::cut)
	/// does for its group.
	///
	/// Bitrate is averaged over whole spans so a lone keyframe is never reported as the track
	/// bitrate on its own. A boundary with no usable duration (an unbounded cut, a span holding one
	/// frame) leaves the span open to fold into the next one, so nothing is lost by cutting often.
	pub fn cut(&mut self, end: Option<Timestamp>) {
		self.bitrate.cut(end.map(nanos));
	}

	/// Discard the open span and the last frame time, so nothing is measured across a break in the
	/// timeline. See [`container::Producer::discontinuity`](crate::container::Producer::discontinuity).
	pub fn discontinuity(&mut self) {
		self.bitrate.discontinuity();
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

	/// Everything measured so far.
	///
	/// Catalog-owned [`container::Producer`](crate::container::Producer) handles publish this
	/// automatically when they cut or finish a group.
	pub fn estimate(&self) -> Estimate {
		Estimate {
			jitter: self.jitter.current(),
			bitrate: self.bitrate.current(),
		}
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
/// [`BITRATE_WINDOW`] of media so a lone keyframe doesn't spike the reported value.
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

		if self.window_duration < BITRATE_WINDOW {
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

/// The largest measured flush delay, batch span, or reorder delay. Once advertised it never falls.
#[derive(Default)]
struct Jitter {
	max: Duration,
}

impl Jitter {
	fn current(&self) -> Option<Duration> {
		(!self.max.is_zero()).then_some(self.max)
	}
}

/// The minimum encode lateness seen anywhere in one broadcast over the recent window.
///
/// A monotonic deque makes observation and expiration amortized constant time. It is shared
/// across renditions so a consistently slower encoder cannot establish its own zero offset.
#[derive(Default)]
pub(crate) struct Baseline {
	samples: VecDeque<Sample>,
}

struct Sample {
	now: u128,
	lateness: i128,
}

impl Baseline {
	fn observe(&mut self, timestamp: u128, now: u128) -> Duration {
		let cutoff = now.saturating_sub(JITTER_WINDOW.as_nanos());
		while self.samples.front().is_some_and(|sample| sample.now < cutoff) {
			self.samples.pop_front();
		}

		let lateness = now as i128 - timestamp as i128;
		while self.samples.back().is_some_and(|sample| sample.lateness >= lateness) {
			self.samples.pop_back();
		}
		self.samples.push_back(Sample { now, lateness });
		let minimum = self.samples.front().expect("the current sample was inserted").lateness;
		let spread = u64::try_from(lateness - minimum).unwrap_or(u64::MAX);
		Duration::from_nanos(spread)
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

	#[test]
	fn batch_flush_at_its_end_reports_its_media_span() {
		let mut estimator = Estimator::new();
		estimator.observe_flush_at(0, 0);
		estimator.observe_flush_at(0, 120_000_000);
		estimator.observe_flush_at(40_000_000, 120_000_000);
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(120)));
	}

	#[test]
	fn two_renditions_share_the_minimum_but_keep_separate_maxima() {
		let baseline = Arc::new(Mutex::new(Baseline::default()));
		let mut audio = Estimator::with_baseline(baseline.clone());
		let mut video = Estimator::with_baseline(baseline);
		audio.observe_flush_at(0, 0);
		video.observe_flush_at(0, 200_000_000);
		assert_eq!(audio.estimate().jitter, None);
		assert_eq!(video.estimate().jitter, Some(Duration::from_millis(200)));
	}

	#[test]
	fn shared_baseline_exposes_offset_and_expires_drift() {
		let mut baseline = Baseline::default();
		assert_eq!(baseline.observe(0, 0), Duration::ZERO);
		assert_eq!(baseline.observe(0, 200_000_000), Duration::from_millis(200));
		assert_eq!(baseline.observe(240_000_000, 240_000_000), Duration::ZERO);
		assert_eq!(baseline.observe(240_000_000, 440_000_000), Duration::from_millis(200));

		let mut drift = Baseline::default();
		let maximum = (0..100u128)
			.map(|second| drift.observe(second * 1_000_000_000, second * 1_001_000_000))
			.max()
			.unwrap();
		assert!(maximum <= Duration::from_millis(10), "{maximum:?}");
	}

	#[test]
	fn early_flush_keeps_lowering_the_baseline() {
		let mut baseline = Baseline::default();
		for second in 0..100u128 {
			assert_eq!(baseline.observe(second * 2_000_000_000, second * 1_000_000_000), Duration::ZERO);
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
}
