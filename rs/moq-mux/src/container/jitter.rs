use std::time::Duration;

use moq_net::Timestamp;

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
	pub fn new() -> Self {
		Self::default()
	}

	/// Record a frame's presentation timestamp (decode order), updating the minimum frame
	/// duration. Returns the new jitter as a [`Duration`] if it changed, else `None`. The
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
	/// jitter as a [`Duration`] if it changed, else `None`.
	pub fn observe_reorder(&mut self, reorder: Timestamp) -> Option<Duration> {
		self.max_reorder = self.max_reorder.max(Duration::from(reorder));
		self.report()
	}

	/// The current jitter (the larger of the frame duration and the reorder delay), without
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

	/// Report the current jitter only when it changes.
	fn report(&mut self) -> Option<Duration> {
		let jitter = self.combined();
		if jitter.is_zero() || self.reported == Some(jitter) {
			return None;
		}
		self.reported = Some(jitter);
		Some(jitter)
	}
}
