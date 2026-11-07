//! Comfort noise for a silence the publisher declared, by the rule in
//! `doc/concept/audio-jitter.md`, which `@moq/watch` follows too.

/// The loudest comfort noise, in RMS: -60 dBFS. Audio that never falls quiet,
/// such as music, would otherwise fill a pause with noise as loud as itself.
const MAX_LEVEL: f64 = 0.001;

/// White noise at the level of the quietest stretch played before a declared
/// silence, so a muted talker's background continues instead of the line going
/// dead. The publisher sends nothing while muted, so the level is measured from
/// what this reader played: the quietest 20 ms block since playback last
/// resumed. A source that was already silent there, such as a noise-suppressed
/// microphone, gets none.
///
/// Allocates nothing, so it runs on the audio thread.
pub(crate) struct Comfort {
	channels: usize,
	/// Frames in a block.
	block: usize,
	/// The block being measured: summed squares across channels, and the frames in it so far.
	sum: f64,
	frames: usize,
	/// The mean square of the quietest whole block since playback last resumed.
	quietest: f64,
	/// The RMS of the noise for the current silence.
	level: f64,
	filling: bool,
	/// xorshift32 state, never zero.
	seed: u32,
	/// Frames filled with noise, ever.
	filled: u64,
}

impl Comfort {
	pub(crate) fn new(rate: u32, channels: usize) -> Self {
		Self {
			channels,
			block: ((rate as f64 / 50.0).round() as usize).max(1),
			sum: 0.0,
			frames: 0,
			quietest: f64::INFINITY,
			level: 0.0,
			filling: false,
			seed: 0x9e37_79b9,
			filled: 0,
		}
	}

	/// Measure interleaved `output`, just played.
	pub(crate) fn played(&mut self, output: &[f32]) {
		if output.len() < self.channels {
			return;
		}
		if std::mem::take(&mut self.filling) {
			self.quietest = f64::INFINITY;
			self.sum = 0.0;
			self.frames = 0;
		}
		for frame in output.chunks_exact(self.channels) {
			for &sample in frame {
				let sample = sample as f64;
				self.sum += sample * sample;
			}
			self.frames += 1;
			if self.frames < self.block {
				continue;
			}
			self.quietest = self.quietest.min(self.sum / (self.block * self.channels) as f64);
			self.sum = 0.0;
			self.frames = 0;
		}
	}

	/// Frames [`fill`](Self::fill) has filled, ever.
	pub(crate) fn filled(&self) -> u64 {
		self.filled
	}

	/// Fill interleaved `output` with comfort noise.
	pub(crate) fn fill(&mut self, output: &mut [f32]) {
		if !std::mem::replace(&mut self.filling, true) && self.quietest.is_finite() {
			// A silence after too little audio to measure keeps the previous level.
			self.level = self.quietest.sqrt().min(MAX_LEVEL);
		}
		self.filled += (output.len() / self.channels) as u64;
		// Uniform noise on [-a, a] has an RMS of a / sqrt(3).
		let amplitude = (self.level * 3f64.sqrt()) as f32;
		for sample in output {
			self.seed ^= self.seed << 13;
			self.seed ^= self.seed >> 17;
			self.seed ^= self.seed << 5;
			*sample = (self.seed as f32 / u32::MAX as f32 * 2.0 - 1.0) * amplitude;
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(serde::Deserialize)]
	struct Case {
		name: String,
		rate: u32,
		channels: usize,
		steps: Vec<Step>,
	}

	#[derive(serde::Deserialize)]
	#[serde(untagged)]
	enum Step {
		Play { play: Vec<f32>, frames: usize },
		Fill { fill: f64 },
	}

	/// Each case plays constant runs and checks the level the next silence fills at.
	#[test]
	fn the_level_conforms_to_the_corpus() {
		let cases: Vec<Case> =
			serde_json::from_str(include_str!("../../../doc/concept/audio-jitter/comfort/corpus.json")).unwrap();
		for case in cases {
			let mut comfort = Comfort::new(case.rate, case.channels);
			for (index, step) in case.steps.iter().enumerate() {
				match step {
					Step::Play { play, frames } => comfort.played(&play.repeat(*frames)),
					Step::Fill { fill } => {
						let mut noise = vec![0.0; 2 * case.channels];
						comfort.fill(&mut noise);
						assert!(
							(comfort.level - fill).abs() < 1e-9,
							"{} step {index}: level {}, want {fill}",
							case.name,
							comfort.level
						);
					}
				}
			}
		}
	}

	/// The noise is white and uniform, so its RMS is the level and nothing exceeds a / sqrt(3).
	#[test]
	fn the_noise_has_the_level_it_was_measured_at() {
		let mut comfort = Comfort::new(48_000, 2);
		comfort.played(&[0.0002; 960 * 2]);
		let mut noise = vec![0.0; 48_000 * 2];
		comfort.fill(&mut noise);
		let peak = noise.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
		let rms = (noise.iter().map(|&sample| sample as f64 * sample as f64).sum::<f64>() / noise.len() as f64).sqrt();
		assert!(peak <= 0.0002 * 3f32.sqrt() * 1.0001, "peak {peak}");
		assert!((rms - 0.0002).abs() < 0.000_005, "rms {rms}");
	}
}
