//! Received-PCM adaptation of WebRTC NetEq's accelerate and preemptive expand.
//! See doc/concept/audio-jitter.md for the shared floating-point algorithm.
//!
//! Copyright (c) 2012 The WebRTC project authors. All Rights Reserved.
//! Ported from modules/audio_coding/neteq/{time_stretch,accelerate,
//! preemptive_expand,dsp_helper,audio_vector,decision_logic}.
//! See WEBRTC-LICENSE and WEBRTC-PATENTS in this crate.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
	Hold,
	Accelerate,
	Fast,
	Expand,
}

/// One bounded block and its seam, allocated before entering the callback.
pub(crate) struct Stretch {
	pub input: Vec<f32>,
	output: Vec<f32>,
	channels: usize,
	rate: u32,
	pub window: usize,
	position: usize,
	length: usize,
	cooldown: usize,
}

impl Stretch {
	pub fn new(rate: u32, channels: usize) -> Self {
		let window = (rate as usize * 30 / 1000).max(1);
		Self {
			input: vec![0.0; window * channels],
			output: vec![0.0; (window + rate as usize * 15 / 1000) * channels],
			channels,
			rate,
			window,
			position: 0,
			length: 0,
			cooldown: rate as usize * 60 / 1000,
		}
	}

	pub fn available(&self) -> usize {
		self.length - self.position
	}

	pub fn discard(&mut self, frames: usize) -> usize {
		let frames = frames.min(self.available());
		self.position += frames;
		frames
	}

	pub fn read(&mut self, output: &mut [f32]) -> usize {
		let frames = (output.len() / self.channels).min(self.available());
		output[..frames * self.channels]
			.copy_from_slice(&self.output[self.position * self.channels..(self.position + frames) * self.channels]);
		self.discard(frames);
		self.cooldown = self.cooldown.saturating_sub(frames);
		frames
	}

	/// All input is new, received PCM. Short blocks and finished sinks cannot
	/// stretch, so a finite input never becomes concealment of an absent frame.
	pub fn process(&mut self, frames: usize, buffered: usize, target: usize, enabled: bool) {
		let operation = if enabled && frames == self.window {
			self.decide(buffered, target)
		} else {
			Operation::Hold
		};
		let lag = if operation == Operation::Hold {
			0
		} else {
			pitch(&self.input, self.channels, self.rate, operation == Operation::Fast)
		};
		self.position = 0;
		self.length = frames;
		if lag == 0 {
			self.output[..frames * self.channels].copy_from_slice(&self.input[..frames * self.channels]);
			return;
		}

		let middle = self.rate as usize * 15 / 1000;
		let (seam, first, second, tail) = match operation {
			Operation::Expand => {
				self.length += lag;
				(middle, middle, middle - lag, middle)
			}
			Operation::Accelerate | Operation::Fast => {
				self.length -= lag;
				(middle - lag, middle - lag, middle, middle + lag)
			}
			Operation::Hold => unreachable!(),
		};
		let channels = self.channels;
		self.output[..seam * channels].copy_from_slice(&self.input[..seam * channels]);
		for frame in 0..lag {
			// NetEq's AudioVector::CrossFade, in floating point. The endpoints
			// stay inside the seam rather than duplicating either neighbour.
			let weight = (frame + 1) as f64 / (lag + 1) as f64;
			for channel in 0..channels {
				let a = self.input[(first + frame) * channels + channel] as f64;
				let b = self.input[(second + frame) * channels + channel] as f64;
				self.output[(seam + frame) * channels + channel] = (a * (1.0 - weight) + b * weight) as f32;
			}
		}
		self.output[(seam + lag) * channels..self.length * channels]
			.copy_from_slice(&self.input[tail * channels..frames * channels]);
		self.cooldown = self.rate as usize * 50 / 1000;
	}

	fn decide(&self, buffered: usize, target: usize) -> Operation {
		let high = target + self.rate as usize * 20 / 1000;
		if buffered >= high * 4 {
			Operation::Fast
		} else if self.cooldown > 0 {
			Operation::Hold
		} else if buffered >= high {
			Operation::Accelerate
		} else if buffered < target {
			Operation::Expand
		} else {
			Operation::Hold
		}
	}
}

/// Search the first channel at 4 kHz, then validate the full-rate seam.
fn pitch(input: &[f32], channels: usize, rate: u32, fast: bool) -> usize {
	// NetEq's 48 kHz Q12 FIR. Sampling its taps at fractional positions also
	// admits native 44.1/96 kHz devices without altering the search domain.
	const FILTER: [f64; 7] = [1019.0, 390.0, 427.0, 440.0, 427.0, 390.0, 1019.0];
	let sample = |position: f64| {
		let last = input.len() / channels - 1;
		let position = position.clamp(0.0, last as f64);
		let at = position as usize;
		let weight = position - at as f64;
		input[at * channels] as f64 * (1.0 - weight) + input[(at + 1).min(last) * channels] as f64 * weight
	};
	let mut down = [0.0; 110];
	for (index, value) in down.iter_mut().enumerate() {
		for (tap, coefficient) in FILTER.iter().enumerate() {
			*value += coefficient * sample((10 + 12 * index - tap) as f64 * rate as f64 / 48_000.0) / 4096.0;
		}
	}
	let mut correlation = [0.0; 50];
	for (index, value) in correlation.iter_mut().enumerate() {
		let lag = index + 10;
		for frame in 60..110 {
			*value += down[frame] * down[frame - lag];
		}
	}
	let mut peak = 0;
	for index in 1..50 {
		if correlation[index] > correlation[peak] {
			peak = index;
		}
	}
	let offset = if peak == 0 {
		0.0
	} else {
		let left = correlation[peak - 1];
		let center = correlation[peak];
		let right = correlation.get(peak + 1).copied().unwrap_or(0.0);
		let denominator = left - 2.0 * center + right;
		if denominator == 0.0 {
			0.0
		} else {
			(0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
		}
	};
	let middle = rate as usize * 15 / 1000;
	if middle == 0 {
		return 0;
	}
	let lag = (((peak + 10) as f64 + offset) * rate as f64 / 4000.0).round() as usize;
	let lag = lag.clamp(1, middle.max(1));
	let mut energy1 = 0.0;
	let mut energy2 = 0.0;
	let mut cross = 0.0;
	for frame in 0..lag {
		let a = input[(middle - lag + frame) * channels] as f64;
		let b = input[(middle + frame) * channels] as f64;
		energy1 += a * a;
		energy2 += b * b;
		cross += a * b;
	}
	// NetEq's uninitialised-background VAD, converted from signed 16-bit PCM.
	let passive = (energy1 + energy2) / (2 * lag) as f64 <= 8.0 * 75_000.0 / (32768.0 * 32768.0);
	let threshold = if fast { 8192.0 } else { 14746.0 } / 16384.0;
	if !passive && cross <= threshold * (energy1 * energy2).sqrt() {
		return 0;
	}
	if fast { middle / lag * lag } else { lag }
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde::Deserialize;

	#[derive(Deserialize)]
	struct Vector {
		name: String,
		rate: u32,
		channels: usize,
		input: Vec<f32>,
		buffered: usize,
		target: usize,
		cooldown: usize,
		decision: String,
		lag: usize,
		output: Vec<f32>,
	}

	#[test]
	fn shared_pcm_vectors() {
		let vectors: Vec<Vector> =
			serde_json::from_str(include_str!("../../../doc/concept/audio-jitter/stretch/corpus.json")).unwrap();
		for vector in vectors {
			let mut stretch = Stretch::new(vector.rate, vector.channels);
			stretch.input.copy_from_slice(&vector.input);
			stretch.cooldown = vector.cooldown;
			assert_eq!(
				format!("{:?}", stretch.decide(vector.buffered, vector.target)),
				vector.decision,
				"{}",
				vector.name
			);
			stretch.process(stretch.window, vector.buffered, vector.target, true);
			assert_eq!(stretch.length.abs_diff(stretch.window), vector.lag, "{}", vector.name);
			assert_eq!(stretch.length * vector.channels, vector.output.len(), "{}", vector.name);
			for (index, (&actual, &expected)) in stretch.output[..vector.output.len()]
				.iter()
				.zip(&vector.output)
				.enumerate()
			{
				assert!(
					(actual - expected).abs() <= 0.000001,
					"{} sample {index}: {actual} != {expected}",
					vector.name
				);
			}
		}
	}

	#[test]
	fn a_finite_input_cannot_expand_forever() {
		for rate in [8_000, 44_100, 48_000, 96_000] {
			let mut stretch = Stretch::new(rate, 2);
			stretch.input.fill(0.25);
			stretch.cooldown = 0;
			stretch.process(stretch.window, stretch.window, rate as usize, true);
			assert!(stretch.length > stretch.window);
			let mut output = vec![0.0; 2 * rate as usize];
			let (_, allocations) = crate::allocation::measure(|| {
				assert_eq!(stretch.read(&mut output), stretch.length);
				assert_eq!(stretch.read(&mut output), 0);
			});
			assert_eq!(allocations.operations, 0);
			stretch.process(stretch.window, stretch.window, rate as usize, false);
			assert_eq!(stretch.length, stretch.window, "a finished sink drains unchanged");
			stretch.process(stretch.window / 2, stretch.window, rate as usize, true);
			assert_eq!(stretch.length, stretch.window / 2, "a short block drains unchanged");
		}
	}
}
