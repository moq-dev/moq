//! Cumulative performance stats since last emulator reset.
//!
//! Tracks how much wall-clock time is spent on emulation, video encoding,
//! and audio encoding. Published to viewers via the status track.

use std::time::{Duration, Instant};

use serde::Serialize;

/// Accumulates timing stats across emulation frames.
pub struct Stats {
	start: Instant,
	emulation: Duration,
	video: Duration,
	audio: Duration,
	last_tick: Instant,
}

impl Stats {
	pub fn new() -> Self {
		let now = Instant::now();
		Self {
			start: now,
			emulation: Duration::ZERO,
			video: Duration::ZERO,
			audio: Duration::ZERO,
			last_tick: now,
		}
	}

	/// Accumulate one frame's worth of time.
	pub fn tick(&mut self, video_active: bool, audio_active: bool) {
		let now = Instant::now();
		let elapsed = now - self.last_tick;
		self.last_tick = now;

		self.emulation += elapsed;
		if video_active {
			self.video += elapsed;
		}
		if audio_active {
			self.audio += elapsed;
		}
	}

	/// Reset the tick timer (e.g. after a pause so the gap isn't counted).
	pub fn reset_tick(&mut self) {
		self.last_tick = Instant::now();
	}

	pub fn report(&self) -> StatsReport {
		let to_secs = |d: Duration| d.as_secs_f64().round() as u64;
		StatsReport {
			video_secs: to_secs(self.video),
			audio_secs: to_secs(self.audio),
			emulation_secs: to_secs(self.emulation),
			wall_secs: to_secs(self.start.elapsed()),
		}
	}
}

/// Serializable stats snapshot sent to viewers.
#[derive(Serialize, PartialEq, Eq)]
pub struct StatsReport {
	pub video_secs: u64,
	pub audio_secs: u64,
	pub emulation_secs: u64,
	pub wall_secs: u64,
}
