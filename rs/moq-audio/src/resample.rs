//! Sample-rate conversion.
//!
//! Wraps [`rubato`] with a small interleaved-`f32` interface so the
//! producer/consumer doesn't have to convert to planar on every call.
//! The resampler keeps the channel layout unchanged; [`remix`] converts mono
//! and stereo after sample-rate conversion.

use rubato::audioadapter_buffers::direct::SequentialSliceOfVecs;
use rubato::{
	Async, FixedAsync, Resampler as RubatoTrait, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

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

/// Convert between known layouts without assigning positions to discrete channels.
pub(crate) fn remix(samples: &[f32], input: Layout, output: Layout) -> Result<Vec<f32>, Error> {
	validate_remix(input, output)?;
	match (input, output) {
		(input, output) if input == output => Ok(samples.to_vec()),
		(Layout::Mono, Layout::Stereo) => {
			let mut output = Vec::with_capacity(samples.len() * 2);
			for &sample in samples {
				output.extend_from_slice(&[sample, sample]);
			}
			Ok(output)
		}
		(Layout::Stereo, Layout::Mono) => Ok(samples.chunks_exact(2).map(|pair| (pair[0] + pair[1]) * 0.5).collect()),
		_ => Err(Error::Unsupported(format!(
			"cannot convert audio layout {input:?} to {output:?} without speaker positions"
		))),
	}
}

/// Check that [`remix`] can convert between two layouts.
pub(crate) fn validate_remix(input: Layout, output: Layout) -> Result<(), Error> {
	input.validate()?;
	output.validate()?;
	if input == output
		|| matches!(
			(input, output),
			(Layout::Mono, Layout::Stereo) | (Layout::Stereo, Layout::Mono)
		) {
		return Ok(());
	}
	Err(Error::Unsupported(format!(
		"cannot convert audio layout {input:?} to {output:?} without speaker positions"
	)))
}

#[cfg(test)]
mod tests {
	use super::*;

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
}
