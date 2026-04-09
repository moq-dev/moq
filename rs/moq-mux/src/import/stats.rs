use std::time::{Duration, Instant};

/// Cumulative import statistics. Use [`Stats::delta`] to compute per-interval metrics.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Stats {
	pub frames: u64,
	pub keyframes: u64,
	pub bytes: u64,
	pub drift: StatsDrift,
}

impl Stats {
	/// Compute the difference between two cumulative snapshots.
	pub fn delta(&self, prev: &Stats) -> Stats {
		Stats {
			frames: self.frames.saturating_sub(prev.frames),
			keyframes: self.keyframes.saturating_sub(prev.keyframes),
			bytes: self.bytes.saturating_sub(prev.bytes),
			drift: self.drift.delta(&prev.drift),
		}
	}
}

/// Frame-to-frame drift accumulator using absolute drift: `|pts_delta - wall_delta|`.
///
/// Mean is ~0 for a perfectly real-time feed; higher means more jitter.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct StatsDrift {
	pub count: u64,
	pub sum: Duration,
}

impl StatsDrift {
	/// Compute the difference between two cumulative snapshots.
	pub fn delta(&self, prev: &StatsDrift) -> StatsDrift {
		StatsDrift {
			count: self.count.saturating_sub(prev.count),
			sum: self.sum.saturating_sub(prev.sum),
		}
	}

	/// Mean absolute drift per frame, or `None` if no samples.
	pub fn mean(&self) -> Option<Duration> {
		if self.count == 0 {
			return None;
		}
		let nanos = self.sum.as_nanos() / self.count as u128;
		Some(Duration::from_nanos(nanos as u64))
	}
}

/// Tracks wall-clock drift between consecutive frames.
///
/// For each pair of consecutive frames, computes `|pts_delta - wall_delta|`.
#[derive(Default)]
pub(crate) struct DriftTracker {
	last_pts: Option<Duration>,
	last_wall: Option<Instant>,
}

impl DriftTracker {
	/// Record a frame's PTS and return the absolute drift from the previous frame.
	pub fn track(&mut self, pts: Duration) -> Option<Duration> {
		let wall = Instant::now();
		let drift = match (self.last_pts, self.last_wall) {
			(Some(prev_pts), Some(prev_wall)) => {
				let pts_delta = pts.saturating_sub(prev_pts);
				let wall_delta = wall.duration_since(prev_wall);
				Some(pts_delta.abs_diff(wall_delta))
			}
			_ => None,
		};
		self.last_pts = Some(pts);
		self.last_wall = Some(wall);
		drift
	}
}

impl Stats {
	/// Record a single frame, updating all counters.
	pub(crate) fn record_frame(&mut self, bytes: u64, keyframe: bool, drift: Option<Duration>) {
		self.frames += 1;
		self.bytes += bytes;
		if keyframe {
			self.keyframes += 1;
		}
		if let Some(d) = drift {
			self.drift.count += 1;
			self.drift.sum += d;
		}
	}
}
