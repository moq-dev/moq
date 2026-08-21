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

use crate::Error;

/// Sample-rate converter over interleaved `f32` PCM.
pub struct Resampler {
	resampler: Async<f32>,
	chunk_frames: usize,
	/// Output frames per input frame, for sizing the flushed tail.
	ratio: f64,
	/// Output frames the sinc filter holds behind what it has already emitted.
	delay: usize,
	/// Whether any caller input has gone in, since the filter only owes a tail
	/// once it has actually run.
	started: bool,
	channels: usize,
	input_planar: Vec<Vec<f32>>,
	output_planar: Vec<Vec<f32>>,
	output_frames_max: usize,
	pending: Vec<f32>,
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
			ratio,
			delay,
			started: false,
			channels: channels as usize,
			input_planar,
			output_planar,
			output_frames_max,
			pending: Vec::new(),
		})
	}

	/// Input frames buffered from earlier calls, waiting for enough to fill a chunk.
	///
	/// The next output starts with these, so a caller stamping its output has to
	/// reach back this far.
	pub fn pending_frames(&self) -> usize {
		self.pending.len() / self.channels
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
	/// caller never supplied: resampling more afterwards would carry that state
	/// into it, across a gap nothing reported. Ending the stream is the only thing
	/// this can be used for, so that is the only thing it can express.
	pub fn flush(mut self) -> Result<Vec<f32>, Error> {
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
		let wanted = ((pending as f64 * self.ratio).round() as usize + self.delay) * self.channels;

		let mut out = Vec::new();
		while out.len() < wanted {
			self.pending.resize(self.chunk_frames * self.channels, 0.0);
			let produced = self.process(&[])?;
			if produced.is_empty() {
				break;
			}
			out.extend_from_slice(&produced);
		}

		out.truncate(wanted);
		Ok(out)
	}

	/// Resample interleaved `f32` input into interleaved `f32` output.
	///
	/// Returns whatever the resampler can produce given the input and
	/// the chunk size; remaining samples are buffered for the next call.
	pub fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>, Error> {
		if !samples.len().is_multiple_of(self.channels) {
			return Err(Error::Misaligned {
				got: samples.len(),
				expected: samples.len().next_multiple_of(self.channels),
			});
		}

		self.started |= !samples.is_empty();
		self.pending.extend_from_slice(samples);

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

		Ok(out)
	}
}

/// Remix interleaved mono/stereo PCM into the requested channel count.
pub(crate) fn remix(samples: &[f32], input_channels: u32, output_channels: u32) -> Result<Vec<f32>, Error> {
	match (input_channels, output_channels) {
		(1, 1) | (2, 2) => Ok(samples.to_vec()),
		(1, 2) => {
			let mut output = Vec::with_capacity(samples.len() * 2);
			for &sample in samples {
				output.extend_from_slice(&[sample, sample]);
			}
			Ok(output)
		}
		(2, 1) => Ok(samples.chunks_exact(2).map(|pair| (pair[0] + pair[1]) * 0.5).collect()),
		_ => Err(Error::Unsupported(format!(
			"channel remix only supports mono and stereo (got {input_channels} to {output_channels})"
		))),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
		let mut out = r.process(&input).unwrap();
		out.extend(r.process(&vec![0.0; 1024]).unwrap());
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

		let body = r.process(&input).unwrap();
		let tail = r.flush().unwrap();

		let peak = |samples: &[f32]| samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
		assert!(peak(&body) < 0.01, "the sample emerged early: peak {}", peak(&body));
		assert!(peak(&tail) > 0.5, "the tail lost the sample: peak {}", peak(&tail));
	}

	/// The filter owes its tail whether or not anything is buffered, so a track
	/// whose length lands exactly on a chunk boundary has to drain too. At 48 kHz
	/// that is every fifteenth AAC packet, so it is not a corner a real stream
	/// avoids.
	#[test]
	fn flush_drains_on_an_exact_chunk_boundary() {
		let mut r = Resampler::new(44_100, 48_000, 1, 882).unwrap();

		// Exactly two chunks of input, so nothing is left pending.
		let mut input = vec![0.0f32; 1764];
		input[1750] = 1.0;

		let body = r.process(&input).unwrap();
		assert_eq!(r.pending_frames(), 0, "the input should divide evenly");

		let tail = r.flush().unwrap();

		// Not exactly zero: a centred sinc has a precursor, so a trace of the sample
		// leads it into the body. The audio itself is still all in the tail.
		let peak = |samples: &[f32]| samples.iter().fold(0.0f32, |max, s| max.max(s.abs()));
		assert!(peak(&body) < 0.01, "the sample emerged early: peak {}", peak(&body));
		assert!(peak(&tail) > 0.5, "the tail lost the sample: peak {}", peak(&tail));
	}

	#[test]
	fn remix_mono_to_stereo_duplicates_samples() {
		assert_eq!(remix(&[1.0, 2.0], 1, 2).unwrap(), [1.0, 1.0, 2.0, 2.0]);
	}

	#[test]
	fn remix_stereo_to_mono_averages_channels() {
		assert_eq!(remix(&[1.0, 3.0, 2.0, 4.0], 2, 1).unwrap(), [2.0, 3.0]);
	}
}
