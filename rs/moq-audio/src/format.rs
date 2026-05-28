use std::borrow::Cow;

use crate::AudioError;

/// Raw PCM sample format.
///
/// Mirrors the WebCodecs `AudioData.format` enum so callers can pass
/// microphone or speaker buffers across the FFI boundary unchanged.
///
/// Interleaved variants pack samples as `[c0_s0, c1_s0, c0_s1, c1_s1, ...]`.
/// Planar variants pack as `[c0_s0, c0_s1, ..., c1_s0, c1_s1, ...]`.
///
/// See <https://developer.mozilla.org/en-US/docs/Web/API/AudioData/format>.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum AudioFormat {
	U8,
	S16,
	S32,
	/// Default: matches libopus's native interleaved layout.
	#[default]
	F32,
	U8Planar,
	S16Planar,
	S32Planar,
	F32Planar,
}

impl AudioFormat {
	/// Bytes used per single-channel sample.
	pub fn bytes_per_sample(self) -> usize {
		match self {
			Self::U8 | Self::U8Planar => 1,
			Self::S16 | Self::S16Planar => 2,
			Self::S32 | Self::S32Planar | Self::F32 | Self::F32Planar => 4,
		}
	}

	/// Whether channels are stored planar (each channel contiguous) rather than interleaved.
	pub fn is_planar(self) -> bool {
		matches!(
			self,
			Self::U8Planar | Self::S16Planar | Self::S32Planar | Self::F32Planar
		)
	}

	/// Whether the underlying sample type is floating-point.
	pub fn is_float(self) -> bool {
		matches!(self, Self::F32 | Self::F32Planar)
	}

	/// Convert a raw PCM buffer in this format to interleaved `f32` in `[-1.0, 1.0]`.
	///
	/// Returns a [`Cow::Borrowed`] when the input is already interleaved `f32`
	/// (and the byte buffer is aligned), avoiding an allocation. Otherwise
	/// returns a [`Cow::Owned`] holding the converted samples.
	pub fn as_interleaved_f32<'a>(self, data: &'a [u8], channels: u32) -> Result<Cow<'a, [f32]>, AudioError> {
		let channels = channels as usize;
		if channels == 0 {
			return Err(AudioError::Unsupported("channel count must be > 0".into()));
		}

		let bps = self.bytes_per_sample();
		if data.len() % (bps * channels) != 0 {
			return Err(AudioError::Misaligned {
				got: data.len(),
				expected: data.len().next_multiple_of(bps * channels),
			});
		}

		// Fast path: already in the codec's working format and aligned.
		if self == Self::F32 {
			let (head, body, tail) = unsafe { data.align_to::<f32>() };
			if head.is_empty() && tail.is_empty() {
				return Ok(Cow::Borrowed(body));
			}
		}

		Ok(Cow::Owned(self.to_interleaved_f32_owned(data, channels)))
	}

	fn to_interleaved_f32_owned(self, data: &[u8], channels: usize) -> Vec<f32> {
		let total_samples = data.len() / self.bytes_per_sample();
		let frames = total_samples / channels;
		let mut out = vec![0.0f32; total_samples];

		match self {
			Self::F32 => {
				for (i, chunk) in data.chunks_exact(4).enumerate() {
					out[i] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
				}
			}
			Self::F32Planar => {
				for ch in 0..channels {
					let plane = &data[ch * frames * 4..(ch + 1) * frames * 4];
					for (frame, chunk) in plane.chunks_exact(4).enumerate() {
						out[frame * channels + ch] = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
					}
				}
			}
			Self::S16 => {
				for (i, chunk) in data.chunks_exact(2).enumerate() {
					let v = i16::from_le_bytes([chunk[0], chunk[1]]);
					out[i] = (v as f32) / 32768.0;
				}
			}
			Self::S16Planar => {
				for ch in 0..channels {
					let plane = &data[ch * frames * 2..(ch + 1) * frames * 2];
					for (frame, chunk) in plane.chunks_exact(2).enumerate() {
						let v = i16::from_le_bytes([chunk[0], chunk[1]]);
						out[frame * channels + ch] = (v as f32) / 32768.0;
					}
				}
			}
			Self::S32 => {
				for (i, chunk) in data.chunks_exact(4).enumerate() {
					let v = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
					out[i] = (v as f32) / (i32::MAX as f32 + 1.0);
				}
			}
			Self::S32Planar => {
				for ch in 0..channels {
					let plane = &data[ch * frames * 4..(ch + 1) * frames * 4];
					for (frame, chunk) in plane.chunks_exact(4).enumerate() {
						let v = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
						out[frame * channels + ch] = (v as f32) / (i32::MAX as f32 + 1.0);
					}
				}
			}
			Self::U8 => {
				for (i, &b) in data.iter().enumerate() {
					out[i] = (b as f32 - 128.0) / 128.0;
				}
			}
			Self::U8Planar => {
				for ch in 0..channels {
					let plane = &data[ch * frames..(ch + 1) * frames];
					for (frame, &b) in plane.iter().enumerate() {
						out[frame * channels + ch] = (b as f32 - 128.0) / 128.0;
					}
				}
			}
		}

		out
	}

	/// Convert interleaved `f32` PCM to this format's raw byte
	/// representation. Returns owned bytes; integer formats clamp
	/// out-of-range samples rather than wrapping.
	pub fn from_interleaved_f32(self, samples: &[f32], channels: u32) -> Result<Vec<u8>, AudioError> {
		let channels = channels as usize;
		if channels == 0 {
			return Err(AudioError::Unsupported("channel count must be > 0".into()));
		}
		if samples.len() % channels != 0 {
			return Err(AudioError::Misaligned {
				got: samples.len(),
				expected: samples.len().next_multiple_of(channels),
			});
		}

		let frames = samples.len() / channels;
		let mut out = vec![0u8; samples.len() * self.bytes_per_sample()];

		match self {
			Self::F32 => {
				for (i, &s) in samples.iter().enumerate() {
					out[i * 4..i * 4 + 4].copy_from_slice(&s.to_le_bytes());
				}
			}
			Self::F32Planar => {
				for ch in 0..channels {
					let plane = &mut out[ch * frames * 4..(ch + 1) * frames * 4];
					for (frame, chunk) in plane.chunks_exact_mut(4).enumerate() {
						chunk.copy_from_slice(&samples[frame * channels + ch].to_le_bytes());
					}
				}
			}
			Self::S16 => {
				for (i, &s) in samples.iter().enumerate() {
					let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
					out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
				}
			}
			Self::S16Planar => {
				for ch in 0..channels {
					let plane = &mut out[ch * frames * 2..(ch + 1) * frames * 2];
					for (frame, chunk) in plane.chunks_exact_mut(2).enumerate() {
						let v = (samples[frame * channels + ch].clamp(-1.0, 1.0) * 32767.0).round() as i16;
						chunk.copy_from_slice(&v.to_le_bytes());
					}
				}
			}
			Self::S32 => {
				for (i, &s) in samples.iter().enumerate() {
					let v = (s.clamp(-1.0, 1.0) as f64 * i32::MAX as f64).round() as i32;
					out[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
				}
			}
			Self::S32Planar => {
				for ch in 0..channels {
					let plane = &mut out[ch * frames * 4..(ch + 1) * frames * 4];
					for (frame, chunk) in plane.chunks_exact_mut(4).enumerate() {
						let v =
							(samples[frame * channels + ch].clamp(-1.0, 1.0) as f64 * i32::MAX as f64).round() as i32;
						chunk.copy_from_slice(&v.to_le_bytes());
					}
				}
			}
			Self::U8 => {
				for (i, &s) in samples.iter().enumerate() {
					out[i] = ((s.clamp(-1.0, 1.0) * 127.0).round() + 128.0) as u8;
				}
			}
			Self::U8Planar => {
				for ch in 0..channels {
					let plane = &mut out[ch * frames..(ch + 1) * frames];
					for (frame, byte) in plane.iter_mut().enumerate() {
						*byte = ((samples[frame * channels + ch].clamp(-1.0, 1.0) * 127.0).round() + 128.0) as u8;
					}
				}
			}
		}

		Ok(out)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn f32_interleaved_is_borrowed() {
		let samples: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4];
		let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
		let cow = AudioFormat::F32.as_interleaved_f32(&bytes, 2).unwrap();
		assert!(matches!(cow, Cow::Borrowed(_)));
		assert_eq!(cow.as_ref(), samples.as_slice());
	}

	#[test]
	fn s16_interleaved_is_owned_but_close() {
		let samples = vec![-1.0_f32, -0.5, 0.0, 0.5, 0.9999];
		let bytes = AudioFormat::S16.from_interleaved_f32(&samples, 1).unwrap();
		let cow = AudioFormat::S16.as_interleaved_f32(&bytes, 1).unwrap();
		assert!(matches!(cow, Cow::Owned(_)));
		for (a, b) in samples.iter().zip(cow.iter()) {
			assert!((a - b).abs() < 1.0 / 32767.0, "{a} vs {b}");
		}
	}

	#[test]
	fn planar_to_interleaved_orders_correctly() {
		let planar: Vec<f32> = vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6];
		let bytes: Vec<u8> = planar.iter().flat_map(|s| s.to_le_bytes()).collect();
		let cow = AudioFormat::F32Planar.as_interleaved_f32(&bytes, 2).unwrap();
		assert_eq!(cow.as_ref(), &[0.1, 0.4, 0.2, 0.5, 0.3, 0.6]);
	}

	#[test]
	fn s16_clamps_out_of_range() {
		let samples = vec![2.0_f32, -3.0];
		let bytes = AudioFormat::S16.from_interleaved_f32(&samples, 1).unwrap();
		let cow = AudioFormat::S16.as_interleaved_f32(&bytes, 1).unwrap();
		assert!((cow[0] - 0.99997).abs() < 1e-4);
		assert!((cow[1] + 1.0).abs() < 1e-4);
	}

	#[test]
	fn rejects_misaligned_buffer() {
		let result = AudioFormat::S16.as_interleaved_f32(&[0u8; 5], 2);
		assert!(matches!(result, Err(AudioError::Misaligned { .. })));
	}
}
