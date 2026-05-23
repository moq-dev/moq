use std::time::Duration;

use crate::container::Timestamp;

/// Tracks the minimum duration between consecutive frames.
///
/// This is the value reported as `jitter` in the catalog: a player should
/// buffer at least this much before emitting frames. Despite the name "jitter",
/// what we actually record is the *minimum frame duration* observed so far.
#[derive(Default)]
pub struct MinFrameDuration {
	last_timestamp: Option<Timestamp>,
	min_duration: Option<Timestamp>,
}

impl MinFrameDuration {
	pub fn new() -> Self {
		Self::default()
	}

	/// Record a new frame timestamp.
	///
	/// Returns the new minimum-frame-duration as a [`Duration`] if it changed, so
	/// the caller can persist it on the catalog rendition. Returns `None` when this
	/// is the first observation, the timestamps are non-monotonic, the new gap is
	/// no smaller than the recorded minimum, or the timestamp's scale is
	/// unspecified.
	pub fn observe(&mut self, ts: Timestamp) -> Option<Duration> {
		let last = self.last_timestamp.replace(ts)?;
		let duration = ts.checked_sub(last).ok()?;

		if let Some(min) = self.min_duration
			&& duration >= min
		{
			return None;
		}

		self.min_duration = Some(duration);
		Duration::try_from(duration).ok()
	}
}
