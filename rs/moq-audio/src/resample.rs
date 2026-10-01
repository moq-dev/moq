//! Sample-rate conversion.
//!
//! Wraps [`rubato`] with a small interleaved-`f32` interface so the
//! producer/consumer doesn't have to convert to planar on every call.
//! The resampler keeps the channel layout unchanged; [`Remix`] converts between
//! layouts after sample-rate conversion.

use std::f32::consts::FRAC_1_SQRT_2;

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{
	Async, FixedAsync, Resampler as RubatoTrait, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use crate::layout::Speaker;
use crate::{Error, Layout};

#[derive(Debug, thiserror::Error)]
enum BackendError {
	#[error(transparent)]
	Construction(#[from] rubato::ResamplerConstructionError),

	#[error(transparent)]
	Process(#[from] rubato::ResampleError),
}

impl BackendError {
	fn into_public(self) -> Error {
		match self {
			Self::Construction(err) => Error::ResamplerConstruction(err.to_string()),
			Self::Process(err) => Error::Resample(err.to_string()),
		}
	}
}

/// Sample-rate converter over interleaved `f32` PCM.
pub struct Resampler {
	resampler: Async<f32>,
	chunk_frames: usize,
	/// Rate the caller's input arrives at, for walking [`held`](Self::held) over
	/// the frames each chunk consumes.
	input_rate: u32,
	/// Output frames per input frame, for sizing the flushed tail.
	ratio: f64,
	/// Output frames the sinc filter holds behind what it has already emitted.
	delay: usize,
	/// Whether any caller input has gone in, since the filter only owes a tail
	/// once it has actually run.
	started: bool,
	/// Leading output frames still to be dropped: the filter opens by emitting its
	/// own centring delay as silence, which is not audio anyone sent.
	skip: usize,
	channels: usize,
	input_planar: Vec<Vec<f32>>,
	output_planar: Vec<Vec<f32>>,
	output_frames_max: usize,
	pending: Vec<f32>,
	/// Where the oldest input frame still buffered came from, walked forward as
	/// chunks are consumed so it also names where the next input lands once
	/// nothing is buffered. `None` until the first input.
	held: Option<moq_net::Timestamp>,
}

impl Resampler {
	/// Build a resampler that converts from `input_rate` to `output_rate`
	/// for the given channel count.
	///
	/// `chunk_frames` is rubato's fixed input window size (per call to
	/// the underlying resampler). The wrapper buffers caller input until
	/// it has at least one chunk.
	pub fn new(input_rate: u32, output_rate: u32, channels: u32, chunk_frames: usize) -> Result<Self, Error> {
		if chunk_frames == 0 {
			return Err(Error::Unsupported("chunk_frames must be > 0".into()));
		}

		Self::new_inner(input_rate, output_rate, channels, chunk_frames).map_err(BackendError::into_public)
	}

	fn new_inner(input_rate: u32, output_rate: u32, channels: u32, chunk_frames: usize) -> Result<Self, BackendError> {
		let params = SincInterpolationParameters {
			sinc_len: 128,
			f_cutoff: Some(0.95),
			interpolation: SincInterpolationType::Linear,
			oversampling_factor: 128,
			window: WindowFunction::BlackmanHarris2,
		};
		let ratio = output_rate as f64 / input_rate as f64;
		let resampler =
			Async::<f32>::new_sinc(ratio, 1.0, &params, chunk_frames, channels as usize, FixedAsync::Input)?;

		let delay = resampler.output_delay();
		let input_planar = (0..channels as usize).map(|_| vec![0.0f32; chunk_frames]).collect();
		let output_frames_max = resampler.output_frames_max();
		let output_planar = vec![vec![0.0f32; output_frames_max]; channels as usize];

		Ok(Self {
			resampler,
			chunk_frames,
			input_rate,
			ratio,
			delay,
			started: false,
			skip: delay,
			channels: channels as usize,
			input_planar,
			output_planar,
			output_frames_max,
			pending: Vec::new(),
			held: None,
		})
	}

	/// Output frames dropped so far as the filter's startup silence.
	///
	/// The output runs that much shorter than the input it was built from, so a
	/// caller stamping its output has to reach back this far from the buffered
	/// input's source timestamp.
	pub fn skipped(&self) -> usize {
		self.delay - self.skip
	}

	/// Input frames buffered from earlier calls, waiting for enough to fill a chunk.
	pub fn pending_frames(&self) -> usize {
		self.pending.len() / self.channels
	}

	/// Where the input the next output starts with came from.
	///
	/// The resampler works in fixed chunks, so it holds back whatever didn't fill
	/// one and the next output begins with those held frames rather than with the
	/// samples just fed in. This is the stamp they arrived under, taken from
	/// [`process`](Self::process) rather than derived by counting backwards from
	/// the newest one, so a jump in the source timeline moves the audio after it
	/// and leaves the audio before it where it belongs. Once nothing is buffered it
	/// names where the next input lands, which is where a
	/// [`drain`](Self::drain) or [`flush`](Self::flush) tail begins.
	///
	/// `None` until the first input, where the caller's own stamp is the answer.
	pub(crate) fn held_at(&self) -> Option<moq_net::Timestamp> {
		self.held
	}

	/// Drop everything held, buffered input and filter state alike, returning to
	/// the just-constructed state.
	///
	/// The escape hatch for a *reported* discontinuity: where [`flush`](Self::flush)
	/// ends the stream, this starts a new one in place, so audio from before the
	/// gap can't bleed through the filter into audio from after it.
	pub fn reset(&mut self) {
		self.resampler.reset();
		self.pending.clear();
		self.skip = self.delay;
		self.started = false;
		self.held = None;
	}

	/// Resample what is still buffered, ending the stream.
	///
	/// The resampler only consumes whole chunks, so without this the last partial
	/// chunk of a track is never converted and its audio is simply lost. Pads the
	/// chunk out with silence and keeps only the output the real input earned, so
	/// the padding costs a filter tail on the final samples rather than extra
	/// audio.
	///
	/// Takes `self` because that padding runs the filter through silence the
	/// caller never supplied: a stream that continues afterwards is a different
	/// stream, which is what [`drain`](Self::drain) says out loud.
	pub fn flush(mut self) -> Result<Vec<f32>, Error> {
		self.drain()
	}

	/// End the current stream and start a new one in place, returning everything
	/// the old one was still holding.
	///
	/// [`flush`](Self::flush) for a gap: the buffered input and the filter's tail
	/// belong *before* the hole, so they come out as their own audio rather than
	/// being filtered together with whatever follows it. Equivalent to a `flush`
	/// followed by a fresh [`Resampler`], without rebuilding the filter tables.
	pub fn drain(&mut self) -> Result<Vec<f32>, Error> {
		let out = self.drained()?;
		self.reset();
		Ok(out)
	}

	fn drained(&mut self) -> Result<Vec<f32>, Error> {
		// Not `pending == 0`: what the filter owes has nothing to do with what is
		// buffered, so a stream that happens to end on a chunk boundary owes a tail
		// just the same. Only one that never ran owes nothing.
		if !self.started {
			return Ok(Vec::new());
		}

		let pending = self.pending_frames();

		// The filter runs centred, so every output frame is built from input around
		// `delay` frames earlier and it still holds that much real audio no amount of
		// input has pushed out. Ask for that much beyond what the pending input
		// earns, feeding silence until it arrives, or a track converts its own
		// ending into frames nobody reads.
		//
		// Only as much as `process` actually dropped off the front, though. That is
		// the whole delay for a stream long enough to have emitted anything, and
		// nothing at all for one that ended before it filled a chunk, where the skip
		// still lies ahead and comes out of this call's own output.
		let repaid = self.delay - self.skip;
		let wanted = ((pending as f64 * self.ratio).round() as usize + repaid) * self.channels;

		let mut out = Vec::new();
		while out.len() < wanted {
			// An empty result does not mean the filter is done: with a chunk smaller
			// than the delay, a whole chunk's output can disappear into the skip while
			// the audio behind it is still coming. Stop only when a chunk moves
			// neither the output nor the skip, which cannot repeat.
			let skip_before = self.skip;
			self.pending.resize(self.chunk_frames * self.channels, 0.0);
			let produced = self.convert().map_err(BackendError::into_public)?;
			if produced.is_empty() && self.skip == skip_before {
				break;
			}
			out.extend_from_slice(&produced);
		}

		out.truncate(wanted);
		Ok(out)
	}

	/// Resample interleaved `f32` input into interleaved `f32` output.
	///
	/// `at` is where the first of `samples` was presented, so buffered input keeps
	/// its source timestamp across calls.
	///
	/// Returns whatever the resampler can produce given the input and
	/// the chunk size; remaining samples are buffered for the next call.
	pub fn process(&mut self, samples: &[f32], at: moq_net::Timestamp) -> Result<Vec<f32>, Error> {
		if !samples.len().is_multiple_of(self.channels) {
			return Err(Error::Misaligned {
				got: samples.len(),
				expected: samples.len().next_multiple_of(self.channels),
			});
		}

		// Nothing buffered means the output resumes with these samples.
		if self.pending.is_empty() {
			self.held = Some(at);
		}

		self.started |= !samples.is_empty();
		self.pending.extend_from_slice(samples);
		let buffered = self.pending.len();
		let out = self.convert().map_err(BackendError::into_public)?;

		// Earlier calls leave less than one chunk, so consuming any chunk also
		// consumes all their samples. The remainder belongs to this packet.
		// Convert its total consumed duration once to preserve fractional progress.
		if self.pending.len() < buffered {
			let consumed = (samples.len() - self.pending.len()) / self.channels;
			let elapsed =
				moq_net::Timestamp::from_scale(consumed as u64, self.input_rate as u64)?.convert(at.scale())?;
			self.held = Some(at.checked_add(elapsed)?);
		}

		Ok(out)
	}

	/// Convert every whole chunk that is buffered, keeping the remainder.
	fn convert(&mut self) -> Result<Vec<f32>, BackendError> {
		let chunk_samples = self.chunk_frames * self.channels;
		let mut out = Vec::new();
		while self.pending.len() >= chunk_samples {
			for (frame_idx, frame) in self.pending[..chunk_samples].chunks_exact(self.channels).enumerate() {
				for (ch, &sample) in frame.iter().enumerate() {
					self.input_planar[ch][frame_idx] = sample;
				}
			}

			let input = SequentialSliceOfVecs::new(&self.input_planar, self.channels, self.chunk_frames)
				.expect("resampler input buffer dimensions");
			let mut output =
				SequentialSliceOfVecs::new_mut(&mut self.output_planar, self.channels, self.output_frames_max)
					.expect("resampler output buffer dimensions");
			let (_, produced) = self.resampler.process_into_buffer(&input, &mut output, None)?;

			let prev_len = out.len();
			out.resize(prev_len + produced * self.channels, 0.0);
			for frame_idx in 0..produced {
				for ch in 0..self.channels {
					out[prev_len + frame_idx * self.channels + ch] = self.output_planar[ch][frame_idx];
				}
			}

			self.pending.drain(..chunk_samples);
		}

		// Drop the filter's startup silence rather than passing it on as audio. What
		// it costs is paid back by `flush`, which drains the same amount at the end,
		// so the output keeps the duration of the input that produced it.
		if self.skip > 0 {
			let drop = self.skip.min(out.len() / self.channels) * self.channels;
			out.drain(..drop);
			self.skip -= drop / self.channels;
		}

		Ok(out)
	}
}

/// A channel mix from one layout to another, each output channel a weighted sum
/// of the input channels.
///
/// Refuses to give discrete channels speaker positions, though it will drop
/// them from named channels.
///
/// Downmixing uses the ITU-R BS.775 coefficients: center and surrounds fold into
/// the front pair at -3 dB and the LFE is dropped. Upmixing leaves the speakers
/// the input lacks silent. Mono is the exception both ways: it plays at full
/// level from both front speakers when there is no center, and a mono output
/// averages the stereo downmix.
pub(crate) struct Remix {
	inputs: usize,
	outputs: usize,
	/// One row of `inputs` weights per output channel.
	weights: Vec<f32>,
}

impl Remix {
	pub(crate) fn new(input: Layout, output: Layout) -> Result<Self, Error> {
		input.validate()?;
		output.validate()?;

		let (inputs, outputs) = (input.channels() as usize, output.channels() as usize);
		// Dropping speaker positions is always safe; inventing them is not.
		let unchanged = input == output || output == Layout::Discrete(inputs as u32);
		let weights = match (input.speakers(), output.speakers()) {
			_ if unchanged => (0..outputs)
				.flat_map(|o| (0..inputs).map(move |i| if i == o { 1.0 } else { 0.0 }))
				.collect(),
			(Some(from), Some(to)) => weights(from, to),
			_ => {
				return Err(Error::Unsupported(format!(
					"cannot convert audio layout {input:?} to {output:?} without speaker positions"
				)));
			}
		};

		Ok(Self {
			inputs,
			outputs,
			weights,
		})
	}

	/// Mix whole interleaved input frames into `output`, which holds as many
	/// frames. Never allocates, so the audio thread can call it.
	pub(crate) fn apply(&self, input: &[f32], output: &mut [f32]) {
		for (frame, out) in input
			.chunks_exact(self.inputs)
			.zip(output.chunks_exact_mut(self.outputs))
		{
			for (sample, row) in out.iter_mut().zip(self.weights.chunks_exact(self.inputs)) {
				*sample = row.iter().zip(frame).map(|(weight, input)| weight * input).sum();
			}
		}
	}

	/// Mix whole interleaved input frames into a new buffer.
	pub(crate) fn process(&self, input: &[f32]) -> Vec<f32> {
		let mut output = vec![0.0; input.len() / self.inputs * self.outputs];
		self.apply(input, &mut output);
		output
	}
}

/// The weights mixing `input` speakers into `output` speakers, one row per output.
fn weights(input: &[Speaker], output: &[Speaker]) -> Vec<f32> {
	use Speaker::*;

	// The only layout without a front pair. Average the stereo downmix rather
	// than invent a center weight for every speaker.
	if output == [FrontCenter] && input != [FrontCenter] {
		let stereo = weights(input, &[FrontLeft, FrontRight]);
		let (left, right) = stereo.split_at(input.len());
		return left.iter().zip(right).map(|(l, r)| (l + r) * 0.5).collect();
	}

	let mut weights = vec![0.0; output.len() * input.len()];
	let has = |speaker| output.contains(&speaker);

	for (i, &speaker) in input.iter().enumerate() {
		let mut feed = |to: Speaker, weight: f32| {
			if let Some(o) = output.iter().position(|s| *s == to) {
				weights[o * input.len() + i] += weight;
			}
		};

		if has(speaker) {
			feed(speaker, 1.0);
			continue;
		}

		match speaker {
			FrontCenter => {
				let weight = if input == [FrontCenter] { 1.0 } else { FRAC_1_SQRT_2 };
				feed(FrontLeft, weight);
				feed(FrontRight, weight);
			}
			Lfe => {}
			SideLeft | BackLeft | SideRight | BackRight => {
				let (front, side, back) = match speaker {
					SideLeft | BackLeft => (FrontLeft, SideLeft, BackLeft),
					_ => (FrontRight, SideRight, BackRight),
				};
				// Side and back are the same surround to a layout with only one of them.
				let other = if speaker == side { back } else { side };
				if has(other) {
					feed(other, 1.0);
				} else {
					feed(front, FRAC_1_SQRT_2);
				}
			}
			BackCenter => {
				if has(BackLeft) {
					feed(BackLeft, FRAC_1_SQRT_2);
					feed(BackRight, FRAC_1_SQRT_2);
				} else if has(SideLeft) {
					feed(SideLeft, FRAC_1_SQRT_2);
					feed(SideRight, FRAC_1_SQRT_2);
				} else {
					feed(FrontLeft, 0.5);
					feed(FrontRight, 0.5);
				}
			}
			// Every output but mono, handled above, has a front pair.
			FrontLeft | FrontRight => unreachable!("{output:?} has no front pair"),
		}
	}

	weights
}

#[cfg(test)]
mod tests {
	use super::*;

	fn remix(samples: &[f32], input: Layout, output: Layout) -> Result<Vec<f32>, Error> {
		Ok(Remix::new(input, output)?.process(samples))
	}

	/// `frames` into a stream at `rate`, as a timestamp in the source's own scale.
	fn at(frames: u64, rate: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_scale(frames, rate).unwrap()
	}

	#[test]
	fn rejects_zero_chunk_frames() {
		let r = Resampler::new(48_000, 48_000, 2, 0);
		assert!(matches!(r, Err(Error::Unsupported(_))));
	}

	#[test]
	fn upsample_44100_to_48000_preserves_energy_roughly() {
		let mut r = Resampler::new(44_100, 48_000, 1, 1024).unwrap();
		let input: Vec<f32> = (0..44_100)
			.map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44_100.0).sin() * 0.5)
			.collect();
		let mut out = r.process(&input, at(0, 44_100)).unwrap();
		out.extend(r.process(&vec![0.0; 1024], at(44_100, 44_100)).unwrap());
		assert!(
			(47_000..50_000).contains(&out.len()),
			"expected ~48k samples, got {}",
			out.len()
		);
	}

	/// The sinc filter is centred, so the end of a track only reaches the output
	/// once further input has passed through it. Without draining that, a track
	/// converts its own ending into frames nobody ever reads, and the tail comes
	/// out silent however loud it was.
	#[test]
	fn flush_drains_the_delayed_tail() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();

		// A full-scale sample near the end of the track, silence around it. Not the
		// very last one: draining stops at the filter's centre rather than emitting
		// its ringing past the end of the signal, so the final sample keeps only
		// half its response however far this drains.
		let mut input = vec![0.0f32; 1024];
		input[1000] = 1.0;

		let body = r.process(&input, at(0, 44_100)).unwrap();
		let tail = r.flush().unwrap();

		let peak = |samples: &[f32]| samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
		assert!(peak(&body) < 0.01, "the sample emerged early: peak {}", peak(&body));
		assert!(peak(&tail) > 0.5, "the tail lost the sample: peak {}", peak(&tail));
	}

	/// The filter owes its tail whether or not anything is buffered, so a track
	/// whose length lands exactly on a chunk boundary has to drain too. With
	/// 1024-sample frames at 48 kHz that lands every fifteenth one against the
	/// 960-frame chunk, so it is not a corner a real stream avoids.
	#[test]
	fn flush_drains_on_an_exact_chunk_boundary() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();

		// Exactly two chunks of input, so nothing is left pending.
		let mut input = vec![0.0f32; 1764];
		input[1750] = 1.0;

		let body = r.process(&input, at(0, 44_100)).unwrap();
		assert_eq!(r.pending_frames(), 0, "the input should divide evenly");

		let tail = r.flush().unwrap();

		// Not exactly zero: a centred sinc has a precursor, so a trace of the sample
		// leads it into the body. The audio itself is still all in the tail.
		let peak = |samples: &[f32]| samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
		assert!(peak(&body) < 0.01, "the sample emerged early: peak {}", peak(&body));
		assert!(peak(&tail) > 0.5, "the tail lost the sample: peak {}", peak(&tail));
	}

	/// A stream that ends before it fills a chunk never emitted anything, so the
	/// filter's startup silence is still ahead of it and comes out of the flush's
	/// own output. Repaying a skip that has not happened yet hands back a stream
	/// longer than its source.
	#[test]
	fn flush_sizes_a_stream_shorter_than_a_chunk() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();

		let body = r.process(&[0.25f32; 441], at(0, 44_100)).unwrap();
		let tail = r.flush().unwrap();

		// 441 frames at 44.1 kHz is 480 at 48 kHz, and that is all it can be.
		let total = body.len() + tail.len();
		assert!((475..=485).contains(&total), "unexpected total: {total}");
	}

	/// `chunk_frames` is the caller's to choose, and a small one can be shorter
	/// than the filter's delay. Then a whole chunk's output disappears into the
	/// startup skip, which used to read as "the filter is done" and drop the
	/// entire stream.
	#[test]
	fn flush_survives_a_chunk_smaller_than_the_delay() {
		let mut r = Resampler::new(44_100, 48_000, 1, 32).unwrap();

		let body = r.process(&[0.5f32; 20], at(0, 44_100)).unwrap();
		let tail = r.flush().unwrap();

		let total = body.len() + tail.len();
		assert!((18..=26).contains(&total), "unexpected total: {total}");
		assert!(
			tail.iter().any(|s| s.abs() > 0.25),
			"the stream came back silent: peak {}",
			tail.iter().fold(0.0f32, |m, s| m.max(s.abs()))
		);
	}

	/// A gap ends one stream and starts another through the same filter, so the
	/// drain has to hand back everything `flush` would and then be usable again,
	/// with none of the first stream's audio reaching the second.
	#[test]
	fn drain_ends_the_stream_and_starts_a_new_one() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();

		let mut input = vec![0.0f32; 1024];
		input[1000] = 1.0;
		let body = r.process(&input, at(0, 44_100)).unwrap();
		let tail = r.drain().unwrap();

		let peak = |samples: &[f32]| samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
		assert!(peak(&tail) > 0.5, "the tail lost the sample: peak {}", peak(&tail));
		assert!(
			(1105..=1120).contains(&(body.len() + tail.len())),
			"unexpected total: {}",
			body.len() + tail.len()
		);

		// Silence in, silence out: nothing is carried over the gap.
		assert_eq!(r.pending_frames(), 0);
		assert_eq!(r.held_at(), None, "the drain should forget where the old stream was");
		let after = r.process(&vec![0.0f32; 1024], at(2048, 44_100)).unwrap();
		assert!(peak(&after) < 0.01, "audio crossed the gap: peak {}", peak(&after));
	}

	/// The output starts with the frames held back from an earlier call, so it
	/// begins where those arrived rather than where the newest input did. Counting
	/// backwards from the newest stamp gets the same answer only while the source
	/// runs contiguous; a jump moves it by the whole jump.
	#[test]
	fn held_frames_keep_the_stamp_they_arrived_under() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();

		// Half a chunk, so all of it is held and nothing comes out.
		assert!(r.process(&[0.25f32; 441], at(0, 44_100)).unwrap().is_empty());
		assert_eq!(r.held_at(), Some(at(0, 44_100)));

		// A second later, and the output it completes still starts back at zero.
		assert!(!r.process(&[0.25f32; 441], at(44_100, 44_100)).unwrap().is_empty());
		assert_eq!(
			r.held_at(),
			Some(at(44_541, 44_100)),
			"the tail starts at the end of the last packet consumed"
		);
	}

	/// With nothing buffered the next output starts with the next input, so a
	/// stream that resumes somewhere else stamps from there and not from where the
	/// old one left off.
	#[test]
	fn an_emptied_buffer_re_anchors_on_the_next_input() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();

		r.process(&[0.25f32; 882], at(0, 44_100)).unwrap();
		assert_eq!(r.pending_frames(), 0);

		r.process(&[0.25f32; 441], at(44_100, 44_100)).unwrap();
		assert_eq!(r.held_at(), Some(at(44_100, 44_100)));
	}

	#[test]
	fn leftover_frames_keep_the_new_packet_timestamp() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();
		r.process(&[0.25; 441], at(0, 44_100)).unwrap();
		r.process(&[0.25; 882], at(44_100, 44_100)).unwrap();
		assert_eq!(r.pending_frames(), 441);
		assert_eq!(r.held_at(), Some(at(44_541, 44_100)));
	}

	#[test]
	fn held_timestamp_preserves_fractional_chunk_progress() {
		let mut r = Resampler::new(11_025, 48_000, 1, 220).unwrap();
		r.process(&vec![0.25; 11_025], at(0, 1000)).unwrap();
		assert_eq!(r.pending_frames(), 25);
		assert_eq!(r.held_at(), Some(at(997, 1000)));
	}

	#[test]
	fn remix_mono_to_stereo_duplicates_samples() {
		assert_eq!(
			remix(&[1.0, 2.0], Layout::Mono, Layout::Stereo).unwrap(),
			[1.0, 1.0, 2.0, 2.0]
		);
	}

	#[test]
	fn remix_stereo_to_mono_averages_channels() {
		assert_eq!(
			remix(&[1.0, 3.0, 2.0, 4.0], Layout::Stereo, Layout::Mono).unwrap(),
			[2.0, 3.0]
		);
	}

	/// One frame of 5.1 with a distinct level per speaker, in canonical order.
	const FIVE_ONE: [f32; 6] = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6];

	fn close(got: &[f32], want: &[f32]) {
		assert_eq!(got.len(), want.len(), "{got:?} vs {want:?}");
		for (g, w) in got.iter().zip(want) {
			assert!((g - w).abs() < 1e-6, "{got:?} vs {want:?}");
		}
	}

	#[test]
	fn remix_downmixes_five_one_to_stereo_by_bs775() {
		let h = FRAC_1_SQRT_2;
		let [l, r, c, _lfe, ls, rs] = FIVE_ONE;
		close(
			&remix(&FIVE_ONE, Layout::FivePointOne, Layout::Stereo).unwrap(),
			&[l + h * c + h * ls, r + h * c + h * rs],
		);
	}

	#[test]
	fn remix_downmixes_five_one_to_mono_through_stereo() {
		let stereo = remix(&FIVE_ONE, Layout::FivePointOne, Layout::Stereo).unwrap();
		close(
			&remix(&FIVE_ONE, Layout::FivePointOne, Layout::Mono).unwrap(),
			&[(stereo[0] + stereo[1]) * 0.5],
		);
	}

	#[test]
	fn remix_upmixes_stereo_into_the_front_pair() {
		close(
			&remix(&[0.25, 0.75], Layout::Stereo, Layout::FivePointOne).unwrap(),
			&[0.25, 0.75, 0.0, 0.0, 0.0, 0.0],
		);
	}

	#[test]
	fn remix_upmixes_mono_into_the_center() {
		close(
			&remix(&[0.5], Layout::Mono, Layout::FivePointOne).unwrap(),
			&[0.0, 0.0, 0.5, 0.0, 0.0, 0.0],
		);
		// Quad has no center, so mono plays from both fronts as it does in stereo.
		close(
			&remix(&[0.5], Layout::Mono, Layout::Quad).unwrap(),
			&[0.5, 0.5, 0.0, 0.0],
		);
	}

	/// 7.1 to 5.1 folds the back pair into the sides at full level, since a 5.1
	/// surround pair is the only surround it has.
	#[test]
	fn remix_folds_back_into_side_surrounds() {
		let seven = [0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8];
		close(
			&remix(&seven, Layout::SevenPointOne, Layout::FivePointOne).unwrap(),
			&[0.1, 0.2, 0.3, 0.4, 0.5 + 0.7, 0.6 + 0.8],
		);
	}

	#[test]
	fn remix_refuses_positions_it_would_invent() {
		for (input, output) in [
			(Layout::Discrete(6), Layout::Stereo),
			(Layout::Mono, Layout::Discrete(2)),
			(Layout::Discrete(0), Layout::Discrete(0)),
		] {
			assert!(
				matches!(Remix::new(input, output), Err(Error::Unsupported(_))),
				"{input:?} -> {output:?}"
			);
		}

		assert_eq!(
			remix(&[1.0, 2.0, 3.0], Layout::Discrete(3), Layout::Discrete(3)).unwrap(),
			[1.0, 2.0, 3.0]
		);
		assert_eq!(
			remix(&[1.0, 2.0, 3.0], Layout::TwoPointOne, Layout::Discrete(3)).unwrap(),
			[1.0, 2.0, 3.0]
		);
		assert!(Remix::new(Layout::Discrete(3), Layout::TwoPointOne).is_err());
	}

	/// Every pair of named layouts converts, and the mix is sized to the output.
	#[test]
	fn remix_converts_between_every_named_layout() {
		let layouts: Vec<Layout> = (1..=8)
			.map(|n| Layout::from_channels(n).unwrap())
			.chain([Layout::ThreePointZero, Layout::FourPointZero])
			.collect();
		for &input in &layouts {
			for &output in &layouts {
				let frame = vec![0.5; input.channels() as usize * 2];
				let mixed = remix(&frame, input, output).unwrap();
				assert_eq!(mixed.len(), output.channels() as usize * 2, "{input:?} -> {output:?}");
			}
		}
	}
}
