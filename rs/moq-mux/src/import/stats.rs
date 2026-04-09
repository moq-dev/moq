use std::ops::Sub;
use std::time::{Duration, Instant};

/// Cumulative import statistics.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Stats {
	pub frames: u64,
	pub keyframes: u64,
	pub bytes: u64,
	pub drift: StatsDrift,
}

impl Sub for Stats {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		Stats {
			frames: self.frames - rhs.frames,
			keyframes: self.keyframes - rhs.keyframes,
			bytes: self.bytes - rhs.bytes,
			drift: self.drift - rhs.drift,
		}
	}
}

impl Sub for &Stats {
	type Output = Stats;

	fn sub(self, rhs: Self) -> Stats {
		Stats {
			frames: self.frames - rhs.frames,
			keyframes: self.keyframes - rhs.keyframes,
			bytes: self.bytes - rhs.bytes,
			drift: self.drift.clone() - rhs.drift.clone(),
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

impl Sub for StatsDrift {
	type Output = Self;

	fn sub(self, rhs: Self) -> Self {
		StatsDrift {
			count: self.count - rhs.count,
			sum: self.sum - rhs.sum,
		}
	}
}

impl StatsDrift {
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
