//! Sample-rate and channel-count conversion.
//!
//! Wraps [`rubato`] with a small interleaved-`f32` interface so the
//! producer/consumer doesn't have to convert to planar on every call.
//! When input and output match exactly, this collapses to a no-op
//! pass-through.

use rubato::{
	Resampler as RubatoTrait, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction,
};

use crate::AudioError;

/// Sample-rate and channel-count converter over interleaved `f32` PCM.
pub struct Resampler {
	input_rate: u32,
	output_rate: u32,
	input_channels: u32,
	output_channels: u32,
	inner: Option<Inner>,
}

struct Inner {
	resampler: SincFixedIn<f32>,
	chunk_frames: usize,
	// Planar scratch buffers reused across calls — rubato wants Vec<Vec<f32>>.
	input_planar: Vec<Vec<f32>>,
	output_planar: Vec<Vec<f32>>,
	// Leftover interleaved samples carried between calls when the
	// caller's input isn't an exact multiple of `chunk_frames`.
	pending: Vec<f32>,
}

impl Resampler {
	/// Build a resampler that converts from `(input_rate, input_channels)`
	/// to `(output_rate, output_channels)`.
	///
	/// Currently only supports `input_channels == output_channels`. Channel
	/// remapping (mono→stereo upmix, stereo→mono downmix) is a TODO.
	pub fn new(input_rate: u32, output_rate: u32, channels: u32, chunk_frames: usize) -> Result<Self, AudioError> {
		Self::with_channels(input_rate, output_rate, channels, channels, chunk_frames)
	}

	fn with_channels(
		input_rate: u32,
		output_rate: u32,
		input_channels: u32,
		output_channels: u32,
		chunk_frames: usize,
	) -> Result<Self, AudioError> {
		if chunk_frames == 0 {
			return Err(AudioError::Unsupported("chunk_frames must be > 0".into()));
		}

		if input_channels != output_channels {
			return Err(AudioError::Unsupported(format!(
				"channel remapping not implemented ({input_channels} → {output_channels})"
			)));
		}

		if input_rate == output_rate {
			return Ok(Self {
				input_rate,
				output_rate,
				input_channels,
				output_channels,
				inner: None,
			});
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
			1.0, // no async ratio change
			params,
			chunk_frames,
			input_channels as usize,
		)?;

		let input_planar = (0..input_channels as usize)
			.map(|_| vec![0.0f32; chunk_frames])
			.collect();
		let output_planar = resampler.output_buffer_allocate(true);

		Ok(Self {
			input_rate,
			output_rate,
			input_channels,
			output_channels,
			inner: Some(Inner {
				resampler,
				chunk_frames,
				input_planar,
				output_planar,
				pending: Vec::new(),
			}),
		})
	}

	/// Whether this resampler is a no-op (rates and channels match).
	pub fn is_passthrough(&self) -> bool {
		self.inner.is_none()
	}

	pub fn input_rate(&self) -> u32 {
		self.input_rate
	}

	pub fn output_rate(&self) -> u32 {
		self.output_rate
	}

	pub fn input_channels(&self) -> u32 {
		self.input_channels
	}

	pub fn output_channels(&self) -> u32 {
		self.output_channels
	}

	/// Resample `samples` (interleaved `f32`) and return the converted
	/// interleaved `f32` output. The returned buffer may be empty if the
	/// caller hasn't supplied enough input to make a chunk yet — the
	/// remainder is buffered internally.
	pub fn process(&mut self, samples: &[f32]) -> Result<Vec<f32>, AudioError> {
		let Some(inner) = self.inner.as_mut() else {
			return Ok(samples.to_vec());
		};

		let channels = self.input_channels as usize;
		if samples.len() % channels != 0 {
			return Err(AudioError::Misaligned {
				got: samples.len(),
				expected: samples.len().next_multiple_of(channels),
			});
		}

		inner.pending.extend_from_slice(samples);

		let mut out = Vec::new();
		let chunk_samples = inner.chunk_frames * channels;
		while inner.pending.len() >= chunk_samples {
			// Deinterleave one chunk into the planar scratch buffer.
			for (frame_idx, frame) in inner.pending[..chunk_samples].chunks_exact(channels).enumerate() {
				for (ch, &sample) in frame.iter().enumerate() {
					inner.input_planar[ch][frame_idx] = sample;
				}
			}

			let (_, produced) =
				inner
					.resampler
					.process_into_buffer(&inner.input_planar, &mut inner.output_planar, None)?;

			// Reinterleave the resampled output.
			let prev_len = out.len();
			out.resize(prev_len + produced * channels, 0.0);
			for frame_idx in 0..produced {
				for ch in 0..channels {
					out[prev_len + frame_idx * channels + ch] = inner.output_planar[ch][frame_idx];
				}
			}

			inner.pending.drain(..chunk_samples);
		}

		Ok(out)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn passthrough_when_rates_match() {
		let mut r = Resampler::new(48_000, 48_000, 2, 480).unwrap();
		assert!(r.is_passthrough());

		let input = vec![1.0, 2.0, 3.0, 4.0];
		let output = r.process(&input).unwrap();
		assert_eq!(output, input);
	}

	#[test]
	fn upsample_44100_to_48000_preserves_energy_roughly() {
		// 44.1 kHz -> 48 kHz, 1 channel, ~1 second of sine
		let mut r = Resampler::new(44_100, 48_000, 1, 1024).unwrap();
		assert!(!r.is_passthrough());

		let input: Vec<f32> = (0..44_100)
			.map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 44_100.0).sin() * 0.5)
			.collect();

		let mut output = r.process(&input).unwrap();
		// Drain any tail from a final flush by feeding a chunk of silence.
		let pad: Vec<f32> = vec![0.0; 1024];
		output.extend(r.process(&pad).unwrap());

		// Should produce ~48000 + small flushed silence; permit slack
		assert!(
			output.len() > 47_000,
			"expected at least 47k samples after upsampling 44.1k, got {}",
			output.len()
		);
		assert!(
			output.len() < 50_000,
			"expected fewer than 50k samples, got {}",
			output.len()
		);
	}

	#[test]
	fn rejects_unsupported_channel_remap() {
		let r = Resampler::with_channels(48_000, 48_000, 1, 2, 480);
		assert!(matches!(r, Err(AudioError::Unsupported(_))));
	}
}
