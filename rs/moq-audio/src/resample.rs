//! Sample-rate conversion.
//!
//! Wraps [`rubato`] with a small interleaved-`f32` interface so the
//! producer/consumer doesn't have to convert to planar on every call.
//! Currently sample-rate only; channel up/downmix is rejected upstream.

use rubato::{
	Resampler as RubatoTrait, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use crate::AudioError;

/// Sample-rate converter over interleaved `f32` PCM.
pub struct Resampler {
	resampler: SincFixedIn<f32>,
	chunk_frames: usize,
	channels: usize,
	input_planar: Vec<Vec<f32>>,
	output_planar: Vec<Vec<f32>>,
	pending: Vec<f32>,
}

impl Resampler {
	/// Build a resampler that converts from `input_rate` to `output_rate`
	/// for the given channel count.
	///
	/// `chunk_frames` is rubato's fixed input window size (per call to
	/// the underlying resampler). The wrapper buffers caller input until
	/// it has at least one chunk.
	pub fn new(input_rate: u32, output_rate: u32, channels: u32, chunk_frames: usize) -> Result<Self, AudioError> {
		if chunk_frames == 0 {
			return Err(AudioError::Unsupported("chunk_frames must be > 0".into()));
		}

		let params = SincInterpolationParameters {
			sinc_len: 128,
			f_cutoff: 0.95,
			interpolation: SincInterpolationType::Linear,
			oversampling_factor: 128,
			window: WindowFunction::BlackmanHarris2,
		};
		let resampler = SincFixedIn::<f32>::new(
			output_rate as f64 / input_rate as f64,
			1.0,
			params,
			chunk_frames,
			channels as usize,
		)?;

		let input_planar = (0..channels as usize).map(|_| vec![0.0f32; chunk_frames]).collect();
		let output_planar = resampler.output_buffer_allocate(true);

		Ok(Self {
			resampler,
			chunk_frames,
			channels: channels as usize,
			input_planar,
			output_planar,
			pending: Vec::new(),
		})
	}

	/// Resample interleaved `f32` input into interleaved `f32` output.
	///
	/// Returns whatever the resampler can produce given the input and
	/// the chunk size; remaining samples are buffered for the next call.
	pub fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>, AudioError> {
		if samples.len() % self.channels != 0 {
			return Err(AudioError::Misaligned {
				got: samples.len(),
				expected: samples.len().next_multiple_of(self.channels),
			});
		}

		self.pending.extend_from_slice(samples);

		let chunk_samples = self.chunk_frames * self.channels;
		let mut out = Vec::new();
		while self.pending.len() >= chunk_samples {
			for (frame_idx, frame) in self.pending[..chunk_samples].chunks_exact(self.channels).enumerate() {
				for (ch, &sample) in frame.iter().enumerate() {
					self.input_planar[ch][frame_idx] = sample;
				}
			}

			let (_, produced) =
				self.resampler
					.process_into_buffer(&self.input_planar, &mut self.output_planar, None)?;

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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn rejects_zero_chunk_frames() {
		let r = Resampler::new(48_000, 48_000, 2, 0);
		assert!(matches!(r, Err(AudioError::Unsupported(_))));
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
}
