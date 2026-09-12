//! Publisher-side hysteresis for the hang catalog `stalled` flag.
//!
//! The wire already carries [`super::VideoConfig::stalled`]; this is the one
//! detector every first-party catalog producer uses to decide when to set it.
//! A rendition is stalled while demand is active and it is not keeping up: the
//! newest source frame has pulled more than a few frame intervals ahead of the
//! newest frame handed to the transport, or the source has gone quiet for that
//! long. An idle broadcast (camera released, no source) is never stalled.
//!
//! The flag flips through an ordinary catalog update, not per frame: it is set
//! once lag exceeds the threshold and cleared only after a run of on-time
//! frames.

use std::time::Duration;

/// How many frame intervals of lag mark a rendition stalled.
pub const SET_INTERVALS: u32 = 3;

/// Consecutive on-time frames required to clear a stall.
pub const CLEAR_FRAMES: u32 = 3;

/// Frame interval used when the catalog does not advertise a framerate.
pub const DEFAULT_INTERVAL: Duration = Duration::from_millis(33);

/// One observation of a rendition's source versus what the transport has accepted.
#[derive(Clone, Copy, Debug)]
pub struct Sample {
	/// Newest captured/source timestamp minus newest timestamp handed to the transport.
	pub media_lag: Duration,
	/// Wall time since the source last delivered a frame for this rendition.
	pub quiet: Duration,
	/// One frame interval. The set threshold is [`SET_INTERVALS`] of these.
	pub interval: Duration,
	/// Someone is subscribed to this rendition.
	pub demand: bool,
	/// Camera released / no live source. Never stalled.
	pub idle: bool,
}

/// Hysteresis around the catalog `stalled` bit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stalled {
	on: bool,
	recover: u32,
}

impl Stalled {
	/// A detector that starts unstalled.
	pub fn new() -> Self {
		Self::default()
	}

	/// Whether the rendition is currently stalled.
	pub fn get(&self) -> bool {
		self.on
	}

	/// The catalog value: `Some(true)` while stalled, omitted otherwise.
	pub fn flag(&self) -> Option<bool> {
		self.on.then_some(true)
	}

	/// Feed one sample. Returns whether the catalog flag changed.
	pub fn observe(&mut self, sample: Sample) -> bool {
		if sample.idle || !sample.demand {
			return self.clear();
		}

		let interval = interval(sample.interval);
		let lag = sample.media_lag.max(sample.quiet);
		let over = lag > interval.saturating_mul(SET_INTERVALS);
		let on_time = lag <= interval;

		if !self.on {
			if !over {
				return false;
			}
			self.on = true;
			self.recover = 0;
			return true;
		}

		if !on_time {
			self.recover = 0;
			return false;
		}

		self.recover = self.recover.saturating_add(1);
		if self.recover < CLEAR_FRAMES {
			return false;
		}
		self.clear()
	}

	fn clear(&mut self) -> bool {
		self.recover = 0;
		if !self.on {
			return false;
		}
		self.on = false;
		true
	}
}

/// A positive frame interval, falling back to [`DEFAULT_INTERVAL`].
pub fn interval(value: Duration) -> Duration {
	if value.is_zero() { DEFAULT_INTERVAL } else { value }
}

/// Frame interval implied by a catalog framerate, or [`DEFAULT_INTERVAL`].
pub fn interval_from_fps(fps: Option<f64>) -> Duration {
	let Some(fps) = fps.filter(|fps| fps.is_finite() && *fps > 0.0) else {
		return DEFAULT_INTERVAL;
	};
	Duration::from_secs_f64(1.0 / fps)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sample(lag: Duration) -> Sample {
		Sample {
			media_lag: lag,
			quiet: Duration::ZERO,
			interval: Duration::from_millis(33),
			demand: true,
			idle: false,
		}
	}

	fn quiet(gap: Duration) -> Sample {
		Sample {
			media_lag: Duration::ZERO,
			quiet: gap,
			interval: Duration::from_millis(33),
			demand: true,
			idle: false,
		}
	}

	#[test]
	fn idle_is_never_stalled() {
		let mut stalled = Stalled::new();
		assert!(!stalled.observe(Sample {
			idle: true,
			demand: true,
			media_lag: Duration::from_secs(10),
			quiet: Duration::from_secs(10),
			interval: Duration::from_millis(33),
		}));
		assert!(!stalled.get());
	}

	#[test]
	fn no_demand_is_never_stalled() {
		let mut stalled = Stalled::new();
		assert!(!stalled.observe(Sample {
			demand: false,
			idle: false,
			media_lag: Duration::from_secs(10),
			quiet: Duration::from_secs(10),
			interval: Duration::from_millis(33),
		}));
		assert!(!stalled.get());
	}

	#[test]
	fn lag_past_the_threshold_sets_the_flag() {
		let mut stalled = Stalled::new();
		assert!(!stalled.observe(sample(Duration::from_millis(99))));
		assert!(stalled.observe(sample(Duration::from_millis(100))));
		assert_eq!(stalled.flag(), Some(true));
	}

	#[test]
	fn a_quiet_source_sets_the_flag() {
		let mut stalled = Stalled::new();
		assert!(stalled.observe(quiet(Duration::from_millis(100))));
		assert!(stalled.get());
	}

	#[test]
	fn clearing_needs_a_run_of_on_time_frames() {
		let mut stalled = Stalled::new();
		assert!(stalled.observe(sample(Duration::from_millis(200))));

		assert!(!stalled.observe(sample(Duration::from_millis(10))));
		assert!(!stalled.observe(sample(Duration::from_millis(10))));
		assert!(stalled.get());

		assert!(stalled.observe(sample(Duration::from_millis(10))));
		assert!(!stalled.get());
		assert_eq!(stalled.flag(), None);
	}

	#[test]
	fn a_late_frame_resets_recovery() {
		let mut stalled = Stalled::new();
		assert!(stalled.observe(sample(Duration::from_millis(200))));
		assert!(!stalled.observe(sample(Duration::from_millis(10))));
		assert!(!stalled.observe(sample(Duration::from_millis(10))));
		assert!(!stalled.observe(sample(Duration::from_millis(200))));
		assert!(!stalled.observe(sample(Duration::from_millis(10))));
		assert!(!stalled.observe(sample(Duration::from_millis(10))));
		assert!(stalled.get());
		assert!(stalled.observe(sample(Duration::from_millis(10))));
		assert!(!stalled.get());
	}

	#[test]
	fn dropping_demand_clears_immediately() {
		let mut stalled = Stalled::new();
		assert!(stalled.observe(sample(Duration::from_millis(200))));
		assert!(stalled.observe(Sample {
			demand: false,
			idle: false,
			media_lag: Duration::from_millis(200),
			quiet: Duration::ZERO,
			interval: Duration::from_millis(33),
		}));
		assert!(!stalled.get());
	}

	#[test]
	fn releasing_the_camera_clears_immediately() {
		let mut stalled = Stalled::new();
		assert!(stalled.observe(sample(Duration::from_millis(200))));
		assert!(stalled.observe(Sample {
			idle: true,
			demand: true,
			media_lag: Duration::from_millis(200),
			quiet: Duration::ZERO,
			interval: Duration::from_millis(33),
		}));
		assert!(!stalled.get());
	}

	#[test]
	fn on_time_frames_do_not_flip_a_healthy_rendition() {
		let mut stalled = Stalled::new();
		for _ in 0..10 {
			assert!(!stalled.observe(sample(Duration::from_millis(10))));
		}
		assert!(!stalled.get());
	}

	#[test]
	fn a_zero_interval_uses_the_default() {
		let mut stalled = Stalled::new();
		assert!(stalled.observe(Sample {
			media_lag: DEFAULT_INTERVAL.saturating_mul(SET_INTERVALS) + Duration::from_millis(1),
			quiet: Duration::ZERO,
			interval: Duration::ZERO,
			demand: true,
			idle: false,
		}));
		assert!(stalled.get());
	}

	#[test]
	fn interval_from_fps_falls_back() {
		assert_eq!(interval_from_fps(None), DEFAULT_INTERVAL);
		assert_eq!(interval_from_fps(Some(0.0)), DEFAULT_INTERVAL);
		assert_eq!(interval_from_fps(Some(f64::NAN)), DEFAULT_INTERVAL);
		assert_eq!(interval_from_fps(Some(50.0)), Duration::from_millis(20));
	}
}
