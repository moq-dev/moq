use std::time::Duration;

use crate::container::Timestamp;

const BITRATE_WINDOW: Duration = Duration::from_secs(1);

/// Catalog metric updates discovered from frame timing and sizes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Update {
	/// The minimum additional latency, if it changed.
	pub latency_min: Option<Duration>,
	/// The current maximum bitrate in bits per second, if it increased.
	pub bitrate: Option<u64>,
}

impl Update {
	fn latency_min(latency_min: Option<Duration>) -> Self {
		Self {
			latency_min,
			bitrate: None,
		}
	}

	fn bitrate(bitrate: Option<u64>) -> Self {
		Self {
			latency_min: None,
			bitrate,
		}
	}

	/// Apply the detected latency and bitrate to a catalog config in place.
	pub(crate) fn apply(&self, config: &mut impl MetricsTarget) {
		if let Some(latency_min) = self.latency_min {
			config.set_latency_min(moq_net::Time::try_from(latency_min).ok());
		}
		self.apply_bitrate(config);
	}

	/// Apply only the detected bitrate, keeping the maximum seen.
	pub(crate) fn apply_bitrate(&self, config: &mut impl MetricsTarget) {
		if let Some(bitrate) = self.bitrate
			&& config.bitrate().is_none_or(|current| bitrate > current)
		{
			config.set_bitrate(bitrate);
		}
	}
}

/// A catalog rendition config the metrics detector refines in place.
///
/// Implemented for the video and audio configs so [`Update::apply`] works on either.
pub(crate) trait MetricsTarget {
	/// Set the minimum additional latency required by this track.
	fn set_latency_min(&mut self, latency_min: Option<moq_net::Time>);
	/// The current bitrate in bits per second, if known.
	fn bitrate(&self) -> Option<u64>;
	/// Set the bitrate in bits per second.
	fn set_bitrate(&mut self, bitrate: u64);
}

impl MetricsTarget for hang::catalog::VideoConfig {
	fn set_latency_min(&mut self, latency_min: Option<moq_net::Time>) {
		hang::catalog::VideoConfig::set_latency_min(self, latency_min);
	}

	fn bitrate(&self) -> Option<u64> {
		self.bitrate
	}

	fn set_bitrate(&mut self, bitrate: u64) {
		self.bitrate = Some(bitrate);
	}
}

impl MetricsTarget for hang::catalog::AudioConfig {
	fn set_latency_min(&mut self, latency_min: Option<moq_net::Time>) {
		hang::catalog::AudioConfig::set_latency_min(self, latency_min);
	}

	fn bitrate(&self) -> Option<u64> {
		self.bitrate
	}

	fn set_bitrate(&mut self, bitrate: u64) {
		self.bitrate = Some(bitrate);
	}
}

/// Tracks catalog metrics for one media track.
///
/// Latency is updated per frame, while bitrate is updated when the current group is
/// finished. The group boundary keeps a single large keyframe from being reported as the
/// track bitrate on its own.
#[derive(Default)]
pub(crate) struct Metrics {
	jitter: Jitter,
	bitrate: Bitrate,
}

impl Metrics {
	pub fn new() -> Self {
		Self::default()
	}

	/// Record one frame's presentation timestamp and encoded byte count.
	pub fn observe_frame(&mut self, ts: Timestamp, bytes: usize) -> Update {
		self.bitrate.observe_frame(ts, bytes);
		Update::latency_min(self.jitter.observe(ts))
	}

	/// Record a frame's reorder delay (`PTS - DTS`).
	pub fn observe_reorder(&mut self, reorder: Timestamp) -> Update {
		Update::latency_min(self.jitter.observe_reorder(reorder))
	}

	/// Finish the current group, using `next` as the group's end timestamp when known.
	pub fn finish_group(&mut self, next: Option<Timestamp>) -> Update {
		Update::bitrate(self.bitrate.finish_group(next))
	}

	/// The current metrics, without change-detection.
	pub fn current(&self) -> Update {
		Update {
			latency_min: self.jitter.current(),
			bitrate: self.bitrate.current(),
		}
	}
}

#[derive(Default)]
struct Bitrate {
	group: Option<Group>,
	window_bytes: u64,
	window_duration: Duration,
	max: Option<u64>,
	reported: Option<u64>,
}

impl Bitrate {
	fn observe_frame(&mut self, ts: Timestamp, bytes: usize) {
		let group = self.group.get_or_insert(Group {
			start: ts,
			max: ts,
			bytes: 0,
		});

		if ts < group.start {
			group.start = ts;
		}
		if ts > group.max {
			group.max = ts;
		}
		group.bytes = group.bytes.saturating_add(bytes as u64);
	}

	fn finish_group(&mut self, next: Option<Timestamp>) -> Option<u64> {
		let group = self.group.take()?;
		let duration = next
			.and_then(|next| next.checked_sub(group.start).ok())
			.filter(|duration| !duration.is_zero())
			.or_else(|| {
				group
					.max
					.checked_sub(group.start)
					.ok()
					.filter(|duration| !duration.is_zero())
			})?;

		self.window_bytes = self.window_bytes.saturating_add(group.bytes);
		self.window_duration += Duration::from(duration);

		if self.window_duration < BITRATE_WINDOW {
			return None;
		}

		let bitrate = bits_per_second(self.window_bytes, self.window_duration);
		self.window_bytes = 0;
		self.window_duration = Duration::ZERO;

		if self.max.is_none_or(|max| bitrate > max) {
			self.max = Some(bitrate);
		}

		if self.reported != self.max {
			self.reported = self.max;
			return self.max;
		}

		None
	}

	fn current(&self) -> Option<u64> {
		self.max
	}
}

struct Group {
	start: Timestamp,
	max: Timestamp,
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

/// Tracks the minimum catalog latency for a video/audio track: the maximum delay before a
/// frame can be emitted, so a player sizes its buffer to at least this much.
///
/// It reports whichever is larger of two contributions:
/// - the minimum frame duration (the steady inter-frame spacing), and
/// - the reorder delay (`max(PTS - DTS)`), which is non-zero only for reordered (B-frame)
///   streams and which a transmuxer also reuses as the decode-clock reserve.
///
/// A non-reordered stream reports the frame duration; a B-frame stream reports the deeper
/// reorder delay (e.g. up to 3 consecutive B-frames is 3x the frame duration).
///
/// Both contributions are kept as scale-free [`Duration`]s: the inputs are `Timestamp`s that
/// may carry different timescales (frame PTS vs a 90 kHz reorder delay), and `Timestamp`
/// arithmetic panics across scales, so they are converted at the boundary before comparison.
#[derive(Default)]
pub struct Jitter {
	last_timestamp: Option<Timestamp>,
	min_duration: Option<Duration>,
	max_reorder: Duration,
	/// Last value handed back from [`observe`](Self::observe) /
	/// [`observe_reorder`](Self::observe_reorder), so they only report on a change.
	reported: Option<Duration>,
}

impl Jitter {
	/// Record a frame's presentation timestamp (decode order), updating the minimum frame
	/// duration. Returns the new latency as a [`Duration`] if it changed, else `None`. The
	/// first observation and non-monotonic timestamps (B-frames) only update state.
	pub fn observe(&mut self, ts: Timestamp) -> Option<Duration> {
		if let Some(last) = self.last_timestamp.replace(ts)
			&& let Ok(duration) = ts.checked_sub(last)
			&& !duration.is_zero()
		{
			let duration = Duration::from(duration);
			self.min_duration = Some(match self.min_duration {
				Some(min) => min.min(duration),
				None => duration,
			});
		}
		self.report()
	}

	/// Record a frame's reorder delay (`PTS - DTS`), updating the maximum. Returns the new
	/// latency as a [`Duration`] if it changed, else `None`.
	pub fn observe_reorder(&mut self, reorder: Timestamp) -> Option<Duration> {
		self.max_reorder = self.max_reorder.max(Duration::from(reorder));
		self.report()
	}

	/// The current latency (the larger of the frame duration and the reorder delay), without
	/// the change-detection of [`observe`](Self::observe). Used to seed a freshly created
	/// catalog rendition with whatever has accumulated, since per-frame updates before the
	/// rendition exists would otherwise be lost.
	pub fn current(&self) -> Option<Duration> {
		let jitter = self.combined();
		(!jitter.is_zero()).then_some(jitter)
	}

	fn combined(&self) -> Duration {
		self.min_duration.unwrap_or(Duration::ZERO).max(self.max_reorder)
	}

	/// Report the current latency only when it changes.
	fn report(&mut self) -> Option<Duration> {
		let latency_min = self.combined();
		if latency_min.is_zero() || self.reported == Some(latency_min) {
			return None;
		}
		self.reported = Some(latency_min);
		Some(latency_min)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn ts(ms: u64) -> Timestamp {
		Timestamp::from_millis(ms).unwrap()
	}

	#[test]
	fn bitrate_waits_for_group_boundaries_and_reports_max() {
		let mut metrics = Metrics::new();

		assert_eq!(metrics.observe_frame(ts(0), 100_000).bitrate, None);
		assert_eq!(metrics.observe_frame(ts(500), 100_000).bitrate, None);
		assert_eq!(metrics.finish_group(Some(ts(1000))).bitrate, Some(1_600_000));

		metrics.observe_frame(ts(1000), 25_000);
		assert_eq!(metrics.finish_group(Some(ts(2000))).bitrate, None);
		assert_eq!(metrics.current().bitrate, Some(1_600_000));

		metrics.observe_frame(ts(2000), 250_000);
		assert_eq!(metrics.finish_group(Some(ts(3000))).bitrate, Some(2_000_000));
	}

	#[test]
	fn jitter_still_reports_larger_of_frame_duration_and_reorder() {
		let mut metrics = Metrics::new();

		assert_eq!(metrics.observe_frame(ts(0), 1).latency_min, None);
		assert_eq!(
			metrics.observe_frame(ts(33), 1).latency_min,
			Some(Duration::from_millis(33))
		);
		assert_eq!(
			metrics.observe_reorder(ts(100)).latency_min,
			Some(Duration::from_millis(100))
		);
		assert_eq!(metrics.current().latency_min, Some(Duration::from_millis(100)));
	}
}
