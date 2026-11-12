//! Received-PCM adaptation of WebRTC NetEq's accelerate and preemptive expand.
//! See doc/concept/audio-jitter.md for the shared floating-point algorithm.
//!
//! Copyright (c) 2012 The WebRTC project authors. All Rights Reserved.
//! Ported from modules/audio_coding/neteq/{time_stretch,accelerate,
//! preemptive_expand,dsp_helper,audio_vector,decision_logic,buffer_level_filter}.
//! See WEBRTC-LICENSE and WEBRTC-PATENTS in this crate.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
	Hold,
	Accelerate,
	Fast,
	Expand,
}

/// What a window decides on, in frames at the device rate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Inputs {
	/// The playout delay the shared clock plays at.
	pub target: usize,
	/// How far the playhead trailed the newest received audio when it
	/// arrived, less what the stretch removed since: the buffer level with
	/// the publisher's cadence taken out. Filtered before deciding.
	pub level: f64,
	/// Received audio ahead of the playhead.
	pub raw: usize,
	/// How far the playhead trails the shared clock: negative when ahead.
	pub lag: i64,
	/// How far behind the clock audio may fall.
	pub bound: i64,
}

/// The shared decision rule on a filtered level, with the most a seam may
/// change the output by in frames. Accelerating only ever catches a playhead
/// behind the clock up to it; expanding slows one whose buffer runs low, as
/// far as `bound` allows, or one ahead of the clock back onto it. A stall
/// delivers no arrival to lower `level`, so `raw` stops a playhead behind the
/// clock from speeding into it.
pub(crate) fn decide(rate: u32, inputs: Inputs, cooldown: usize) -> (Operation, i64) {
	let Inputs {
		target,
		level,
		raw,
		lag,
		bound,
	} = inputs;
	let rate = rate as usize;
	// NetEq's DecisionLogic limits, with its 85 ms deceleration offset and
	// 20 ms granularity.
	let low = (3 * target / 4).max(target.saturating_sub(rate * 85 / 1000));
	let high = target.max(low + rate * 20 / 1000);
	let buffer = level.min(raw as f64);
	if lag > 0 && buffer >= (4 * high) as f64 {
		(Operation::Fast, lag)
	} else if cooldown > 0 {
		(Operation::Hold, 0)
	} else if lag > 0 && buffer >= high as f64 {
		(Operation::Accelerate, lag)
	} else if level < low as f64 && lag < bound {
		(Operation::Expand, bound - lag)
	} else if lag < 0 {
		(Operation::Expand, -lag)
	} else {
		(Operation::Hold, 0)
	}
}

/// NetEq's buffer-level filter, updated once per window: three of its 10 ms
/// steps, its factor chosen by the target, less what the stretch changed since
/// the last update. Unset until the first update after a start or a
/// [`reset`](Self::reset), which takes the level as it is.
#[derive(Default)]
pub(crate) struct Level {
	filtered: Option<f64>,
}

impl Level {
	/// Fold in `level`, after windows whose stretch removed `stretched` frames
	/// (negative: added).
	pub fn update(&mut self, rate: u32, target: usize, level: f64, stretched: i64) -> f64 {
		let Some(filtered) = self.filtered else {
			self.filtered = Some(level);
			return level;
		};
		// NetEq's BufferLevelFilter factors, in 256ths per 10 ms step, by the
		// target in milliseconds.
		let rate = rate as usize;
		let factor: u64 = if target * 1000 <= 20 * rate {
			251
		} else if target * 1000 <= 60 * rate {
			252
		} else if target * 1000 <= 140 * rate {
			253
		} else {
			254
		};
		let weight = (factor * factor * factor) as f64 / 16_777_216.0;
		let filtered = (weight * filtered + (1.0 - weight) * level - stretched as f64).max(0.0);
		self.filtered = Some(filtered);
		filtered
	}

	/// Forget the history, so the next update starts from its level.
	pub fn reset(&mut self) {
		self.filtered = None;
	}
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
	/// What the stretch removed since the last filter update.
	stretched: i64,
	pub level: Level,
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
			stretched: 0,
			level: Level::default(),
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

	/// All input is new, received PCM. Short blocks and windows without
	/// `inputs` cannot stretch, so a finite input never becomes concealment of
	/// an absent frame.
	pub fn process(&mut self, frames: usize, inputs: Option<Inputs>) -> Operation {
		let Some(inputs) = inputs.filter(|_| frames == self.window) else {
			return self.stage(frames, Operation::Hold, 0);
		};
		let level = self
			.level
			.update(self.rate, inputs.target, inputs.level, self.stretched);
		self.stretched = 0;
		let (operation, allowance) = decide(self.rate, Inputs { level, ..inputs }, self.cooldown);
		self.stage(frames, operation, allowance)
	}

	/// Stage the first `frames` of the input as `operation` decides, if a
	/// pitch period within `allowance` admits it.
	fn stage(&mut self, frames: usize, operation: Operation, allowance: i64) -> Operation {
		let lag = if operation == Operation::Hold {
			0
		} else {
			pitch(&self.input, self.channels, self.rate, operation, allowance)
		};
		self.position = 0;
		self.length = frames;
		if lag == 0 {
			self.output[..frames * self.channels].copy_from_slice(&self.input[..frames * self.channels]);
			return operation;
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
		self.stretched += frames as i64 - self.length as i64;
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
		operation
	}
}

/// Search the first channel at 4 kHz, then validate the full-rate seam,
/// returning how many frames it changes the output by, or zero. Fast mode
/// admits a weaker match and takes every whole period that fits before the
/// middle; the others take one. None takes more than `allowance`.
fn pitch(input: &[f32], channels: usize, rate: u32, operation: Operation, allowance: i64) -> usize {
	let fast = operation == Operation::Fast;
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
	let allowance = usize::try_from(allowance).unwrap_or(0);
	let periods = if fast {
		(middle / lag).min(allowance / lag)
	} else {
		usize::from(allowance >= lag)
	};
	periods * lag
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde::Deserialize;

	#[derive(Deserialize)]
	struct Pcm {
		name: String,
		rate: u32,
		channels: usize,
		input: Vec<f32>,
		decision: String,
		allowance: i64,
		lag: usize,
		output: Vec<f32>,
	}

	#[derive(Deserialize)]
	struct Vectors {
		decisions: Vec<Decision>,
		filter: Vec<Filter>,
	}

	#[derive(Deserialize)]
	struct Decision {
		name: String,
		rate: u32,
		target: usize,
		level: f64,
		raw: usize,
		lag: i64,
		bound: i64,
		cooldown: usize,
		decision: String,
		allowance: i64,
	}

	#[derive(Deserialize)]
	struct Filter {
		name: String,
		rate: u32,
		target: usize,
		steps: Vec<(f64, i64)>,
		filtered: Vec<f64>,
	}

	fn operation(name: &str) -> Operation {
		match name {
			"Hold" => Operation::Hold,
			"Accelerate" => Operation::Accelerate,
			"Fast" => Operation::Fast,
			"Expand" => Operation::Expand,
			_ => panic!("unknown decision {name}"),
		}
	}

	fn vectors() -> Vectors {
		serde_json::from_str(include_str!("../../../doc/concept/audio-jitter/stretch/decisions.json")).unwrap()
	}

	#[test]
	fn shared_pcm_vectors() {
		let vectors: Vec<Pcm> =
			serde_json::from_str(include_str!("../../../doc/concept/audio-jitter/stretch/corpus.json")).unwrap();
		for vector in vectors {
			let mut stretch = Stretch::new(vector.rate, vector.channels);
			stretch.input.copy_from_slice(&vector.input);
			stretch.stage(stretch.window, operation(&vector.decision), vector.allowance);
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
	fn shared_decision_vectors() {
		for vector in vectors().decisions {
			let inputs = Inputs {
				target: vector.target,
				level: vector.level,
				raw: vector.raw,
				lag: vector.lag,
				bound: vector.bound,
			};
			assert_eq!(
				decide(vector.rate, inputs, vector.cooldown),
				(operation(&vector.decision), vector.allowance),
				"{}",
				vector.name
			);
		}
	}

	#[test]
	fn shared_filter_vectors() {
		for vector in vectors().filter {
			let mut level = Level::default();
			for (step, (&(value, stretched), &expected)) in vector.steps.iter().zip(&vector.filtered).enumerate() {
				let actual = level.update(vector.rate, vector.target, value, stretched);
				assert!(
					(actual - expected).abs() <= 1e-9,
					"{} step {step}: {actual} != {expected}",
					vector.name
				);
			}
		}
	}

	#[test]
	fn the_filter_takes_out_what_the_stretch_changed() {
		// 48 kHz at a 100 ms target: a constant signal accelerates by its
		// shortest period, 120 frames, from a buffer at the high limit.
		let inputs = Inputs {
			target: 4800,
			level: 4800.0,
			raw: 4800,
			lag: 120,
			bound: 0,
		};
		let mut stretch = Stretch::new(48_000, 1);
		stretch.input.fill(0.25);
		stretch.cooldown = 0;
		assert_eq!(stretch.process(stretch.window, Some(inputs)), Operation::Accelerate);
		assert_eq!(stretch.length, stretch.window - 120);
		assert_eq!(stretch.level.filtered, Some(4800.0), "the first update takes the level");
		stretch.cooldown = 1;
		stretch.process(stretch.window, Some(inputs));
		assert_eq!(
			stretch.level.filtered,
			Some(4680.0),
			"the next takes out the period removed"
		);
		stretch.level.reset();
		stretch.process(stretch.window, Some(inputs));
		assert_eq!(
			stretch.level.filtered,
			Some(4800.0),
			"a reset starts again from the level"
		);
	}

	#[test]
	fn a_finite_input_cannot_expand_forever() {
		for rate in [8_000, 44_100, 48_000, 96_000] {
			let mut stretch = Stretch::new(rate, 2);
			stretch.input.fill(0.25);
			stretch.cooldown = 0;
			// Ahead of the clock by a second: expand back onto it.
			let inputs = Some(Inputs {
				target: rate as usize / 10,
				level: rate as f64 / 10.0,
				raw: rate as usize / 10,
				lag: -(rate as i64),
				bound: 0,
			});
			assert_eq!(stretch.process(stretch.window, inputs), Operation::Expand);
			assert!(stretch.length > stretch.window);
			let mut output = vec![0.0; 2 * rate as usize];
			let (_, allocations) = crate::allocation::measure(|| {
				assert_eq!(stretch.read(&mut output), stretch.length);
				assert_eq!(stretch.read(&mut output), 0);
			});
			assert_eq!(allocations.operations, 0);
			stretch.cooldown = 0;
			stretch.process(stretch.window, None);
			assert_eq!(stretch.length, stretch.window, "a finished sink drains unchanged");
			stretch.process(stretch.window / 2, inputs);
			assert_eq!(stretch.length, stretch.window / 2, "a short block drains unchanged");
		}
	}

	#[test]
	fn a_seam_never_overshoots_the_allowance() {
		// A constant signal's shortest period is 2.5 ms: 120 frames at 48 kHz.
		for (operation, allowance, length) in [
			(Operation::Accelerate, 119, 1440),
			(Operation::Accelerate, 120, 1320),
			(Operation::Accelerate, 239, 1320),
			(Operation::Fast, 360, 1080),
			(Operation::Expand, 119, 1440),
			(Operation::Expand, 120, 1560),
		] {
			let mut stretch = Stretch::new(48_000, 1);
			stretch.input.fill(0.25);
			stretch.stage(stretch.window, operation, allowance);
			assert_eq!(stretch.length, length, "{operation:?} {allowance}");
		}
	}
}
