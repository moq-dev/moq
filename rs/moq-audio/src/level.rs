/// How loud a stretch of audio was, for drawing a meter.
///
/// Reported by a capture [`Control`](crate::encode::Control) and by a mixing
/// bus and its strips. Read it at whatever rate the meter draws at.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Level {
	rms: f32,
	peak: f32,
}

impl Level {
	/// Root mean square amplitude on a linear `0.0..=1.0` scale.
	pub fn rms(&self) -> f32 {
		self.rms
	}

	/// Peak absolute amplitude on a linear `0.0..=1.0` scale.
	pub fn peak(&self) -> f32 {
		self.peak
	}

	#[cfg_attr(
		not(any(feature = "capture", feature = "bus")),
		allow(dead_code, reason = "only capture and the bus measure levels")
	)]
	pub(crate) fn measure(samples: &[f32]) -> Self {
		if samples.is_empty() {
			return Self::default();
		}

		let mut squares = 0.0f64;
		let mut peak = 0.0f32;
		for &sample in samples {
			let sample = sample.clamp(-1.0, 1.0);
			squares += f64::from(sample) * f64::from(sample);
			peak = peak.max(sample.abs());
		}

		Self {
			rms: (squares / samples.len() as f64).sqrt() as f32,
			peak,
		}
	}

	/// The louder of two readings, channel by channel, so a meter read less often
	/// than buffers arrive still shows the loudest of them.
	#[cfg_attr(not(feature = "bus"), allow(dead_code, reason = "only the bus holds levels"))]
	pub(crate) fn max(self, other: Self) -> Self {
		Self {
			rms: self.rms.max(other.rms),
			peak: self.peak.max(other.peak),
		}
	}
}
