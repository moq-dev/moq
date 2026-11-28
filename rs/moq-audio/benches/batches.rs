//! PCM batching cost through [`encode::Producer`]: one constant duration of audio,
//! delivered in batches from tiny steady writes to a single burst, swept over channels,
//! input rates (with and without resampling) and independent producers.
//!
//! PCM is the codec, so buffering and resampling show instead of codec CPU; four Opus
//! cases put that cost beside a real codec's. Each call gets fresh producers, writes
//! every batch and finishes the track, so the partial tail and resampler flush count too.
//!
//! Run with `cargo bench -p moq-audio --bench batches`.

use std::fmt;

use bytes::Bytes;
use moq_audio::encode::{Codec, Config, Finished, Input, Options, Producer};
use moq_audio::{Frame, Layout};
use moq_net::Timestamp;

/// Every case delivers this much audio, however it is batched.
const SECONDS: usize = 4;
/// The codec rate every case publishes at.
const CODEC_RATE: u32 = 48_000;

#[derive(Clone, Copy)]
struct Case {
	codec: Codec,
	input_rate: u32,
	layout: Layout,
	/// Input frames (per channel) in each write; the last write may be shorter.
	batch: usize,
	producers: usize,
}

impl Case {
	fn frames(&self) -> usize {
		SECONDS * self.input_rate as usize
	}

	fn samples(&self) -> u64 {
		(self.frames() * self.layout.channels() as usize * self.producers) as u64
	}

	/// The batches every producer writes.
	fn input(&self) -> Vec<Frame> {
		let channels = self.layout.channels() as usize;
		let samples: Vec<f32> = (0..self.frames() * channels)
			.map(|index| (index as f32 * 0.01).sin() * 0.25)
			.collect();
		samples
			.chunks(self.batch * channels)
			.enumerate()
			.map(|(index, chunk)| {
				let frames = (index * self.batch) as u64;
				let timestamp = Timestamp::from_micros(frames * 1_000_000 / self.input_rate as u64).unwrap();
				let data: Vec<u8> = chunk.iter().flat_map(|sample| sample.to_le_bytes()).collect();
				Frame::new(Bytes::from(data), timestamp)
			})
			.collect()
	}

	/// Fresh producers on one broadcast, and the batches they write.
	fn batches(self) -> Batches {
		let mut broadcast = moq_net::broadcast::Info::new().produce();
		let catalog = moq_mux::catalog::Producer::new(&mut broadcast, moq_mux::catalog::Config::default()).unwrap();
		let input = Input::new(self.input_rate, self.layout);
		let mut options = Options::default();
		options.config = Config::from_input(self.codec, &input);
		options.config.sample_rate = CODEC_RATE;
		let producers = (0..self.producers)
			.map(|_| Producer::new(&mut broadcast, catalog.clone(), input.clone(), &options).unwrap())
			.collect();
		Batches {
			broadcast,
			producers,
			input: self.input(),
		}
	}
}

impl fmt::Display for Case {
	fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
		let batch = if self.batch >= self.frames() {
			"burst".to_string()
		} else {
			self.batch.to_string()
		};
		write!(
			f,
			"{}/{}hz/{}ch/batch_{batch}/producers_{}",
			self.codec,
			self.input_rate,
			self.layout.channels(),
			self.producers
		)
	}
}

struct Batches {
	broadcast: moq_net::broadcast::Producer,
	producers: Vec<Producer>,
	input: Vec<Frame>,
}

fn cases() -> Vec<Case> {
	let case = |codec, input_rate: u32, layout, batch, producers| Case {
		codec,
		input_rate,
		layout,
		batch,
		producers,
	};
	let rates = [48_000, 44_100, 96_000];
	// 32 frames, 10 ms, 100 ms, one second, then all of it in one write.
	let batches = |rate: u32| {
		let rate = rate as usize;
		[32, rate / 100, rate / 10, rate, SECONDS * rate]
	};

	let mut cases = Vec::new();
	for rate in rates {
		for batch in batches(rate) {
			cases.push(case(Codec::Pcm, rate, Layout::Stereo, batch, 1));
		}
	}
	for layout in [Layout::Mono, Layout::FivePointOne] {
		for rate in rates {
			let [_, steady, _, _, burst] = batches(rate);
			for batch in [steady, burst] {
				cases.push(case(Codec::Pcm, rate, layout, batch, 1));
			}
		}
	}
	for rate in [48_000, 44_100] {
		let [_, steady, _, _, burst] = batches(rate);
		for batch in [steady, burst] {
			cases.push(case(Codec::Pcm, rate, Layout::Stereo, batch, 8));
			cases.push(case(Codec::Opus, rate, Layout::Stereo, batch, 1));
		}
	}
	cases
}

/// Write every batch to every producer, then finish each. Returns the broadcast and what the
/// producers finished, so their teardown stays outside the measurement.
#[swole::bench(args = cases(), setup = Case::batches, items = |case: Case| case.samples())]
fn pcm_batches(batches: Batches) -> (moq_net::broadcast::Producer, Vec<Finished>, Vec<Frame>) {
	let Batches {
		broadcast,
		mut producers,
		input,
	} = batches;
	for frame in &input {
		for producer in producers.iter_mut() {
			producer.write(frame).unwrap();
		}
	}
	let finished = producers
		.into_iter()
		.map(|producer| producer.finish().unwrap())
		.collect();
	(broadcast, finished, input)
}

fn main() {
	swole::main();
}
