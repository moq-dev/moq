use std::time::Duration;

use moq_net::Timestamp;

/// The window over which bitrate is averaged before it is reported.
const BITRATE_WINDOW: Duration = Duration::from_secs(1);

/// The catalog fields an [`Estimator`] can measure from the frames fed to it.
///
/// An absent field is one to measure: whatever the config already carries when it reaches
/// [`Rendition::set`](super::Rendition::set) is authoritative and left alone, and the rest is
/// detected and kept current.
#[derive(Clone, Default, Debug, PartialEq)]
#[non_exhaustive]
pub struct Estimate {
	/// The maximum jitter before the next frame is emitted.
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
/// A [`container::Producer`](crate::container::Producer) owns one and feeds it as you write, so
/// reading the result is all a publisher does:
///
/// ```no_run
/// # fn example<E: moq_mux::catalog::hang::CatalogExt>(
/// #     mut catalog: moq_mux::catalog::Producer<E>,
/// #     reserved: moq_mux::catalog::Reserved<E>,
/// #     net: moq_net::track::Producer,
/// #     config: hang::catalog::VideoConfig,
/// #     frame: moq_mux::container::Frame,
/// # ) -> moq_mux::Result<()> {
/// use moq_mux::catalog::hang::Container;
/// let mut track = catalog.media_producer(net, Container::Legacy)?;
/// let mut rendition = reserved.video(track.name());
/// rendition.set(config);
///
/// track.write(frame)?;
/// rendition.estimate(track.estimate());
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
}

impl Estimator {
	/// Create an empty estimator.
	pub fn new() -> Self {
		Self::default()
	}

	/// Observe a frame of `bytes` encoded bytes at presentation time `timestamp`, as written by
	/// [`container::Producer::write`](crate::container::Producer::write).
	pub fn write(&mut self, timestamp: Timestamp, bytes: usize) {
		let timestamp = nanos(timestamp);
		self.bitrate.write(timestamp, bytes);
		self.jitter.write(timestamp);
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
		self.jitter.discontinuity();
	}

	/// Observe a frame's reorder delay (`PTS - DTS`), which raises the jitter to the decode buffer a
	/// B-frame stream needs. Only a container knows this; the elementary stream carries no decode
	/// time.
	pub fn reorder(&mut self, delay: Timestamp) {
		self.jitter.reorder(delay);
	}

	/// Everything measured so far. Hand it to
	/// [`Rendition::estimate`](super::Rendition::estimate) to publish it.
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

/// Tracks the catalog `jitter` for a video/audio track: the maximum delay before a frame can
/// be emitted, so a player sizes its buffer to at least this much.
///
/// It reports whichever is larger of two contributions:
/// - the minimum frame duration (the steady inter-frame spacing), and
/// - the reorder delay (`max(PTS - DTS)`), which is non-zero only for reordered (B-frame)
///   streams and which a transmuxer also reuses as the decode-clock reserve.
///
/// A non-reordered stream reports the frame duration; a B-frame stream reports the deeper
/// reorder delay (e.g. up to 3 consecutive B-frames is 3x the frame duration).
///
/// Both contributions are kept as [`Duration`]s, since the two inputs are independently scaled
/// (frame PTS vs a 90 kHz reorder delay) and only compare once normalized. See [`nanos`].
#[derive(Default)]
struct Jitter {
	/// Scale-free nanoseconds, per [`nanos`].
	last: Option<u128>,
	min_duration: Option<Duration>,
	max_reorder: Duration,
}

impl Jitter {
	/// Record a frame's presentation timestamp (decode order), updating the minimum frame duration.
	/// The first observation and non-monotonic timestamps (B-frames) only update state.
	fn write(&mut self, ts: u128) {
		if let Some(last) = self.last.replace(ts)
			&& let Some(duration) = elapsed(last, ts)
		{
			self.min_duration = Some(match self.min_duration {
				Some(min) => min.min(duration),
				None => duration,
			});
		}
	}

	fn reorder(&mut self, delay: Timestamp) {
		self.max_reorder = self.max_reorder.max(Duration::from(delay));
	}

	fn discontinuity(&mut self) {
		self.last = None;
	}

	fn current(&self) -> Option<Duration> {
		let jitter = self.min_duration.unwrap_or(Duration::ZERO).max(self.max_reorder);
		(!jitter.is_zero()).then_some(jitter)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn micros(value: u64) -> Timestamp {
		Timestamp::from_micros(value).unwrap()
	}

	#[test]
	fn reports_the_minimum_frame_spacing() {
		let mut estimator = Estimator::new();

		estimator.write(micros(1_000), 1);
		assert_eq!(estimator.estimate().jitter, None, "one frame has no spacing");
		estimator.write(micros(41_000), 1);
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(40)));
		estimator.write(micros(81_000), 1);
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(40)));
		estimator.write(micros(101_000), 1);
		assert_eq!(
			estimator.estimate().jitter,
			Some(Duration::from_millis(20)),
			"the minimum wins"
		);
	}

	#[test]
	fn reorder_delay_wins_over_frame_spacing() {
		let mut estimator = Estimator::new();

		estimator.write(micros(0), 1);
		estimator.write(micros(16_000), 1);
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(16)));

		estimator.reorder(micros(48_000));
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(48)));

		// A B-frame presenting earlier than its predecessor contributes no spacing.
		estimator.write(micros(32_000), 1);
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(48)));
	}

	#[test]
	fn ignores_non_monotonic_presentation_spacing() {
		let mut estimator = Estimator::new();

		estimator.write(micros(100_000), 1);
		estimator.write(micros(80_000), 1);
		assert_eq!(estimator.estimate().jitter, None);
		estimator.write(micros(120_000), 1);
		assert_eq!(estimator.estimate().jitter, Some(Duration::from_millis(40)));
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
		assert_eq!(estimate.jitter, Some(Duration::from_millis(40)));
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
