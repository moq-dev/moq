use bytes::Bytes;

use crate::AudioFormat;

/// A buffer of raw PCM audio samples ready to feed an encoder, or freshly
/// emitted from a decoder.
///
/// Layout of `data` is fully described by `format` and `channel_count`:
/// see [`AudioFormat`] for the interleaved vs planar conventions. The
/// number of frames is implicit:
/// `data.len() / (format.bytes_per_sample() * channel_count)`.
#[derive(Clone, Debug)]
pub struct AudioSamples {
	/// Layout of the bytes in `data`.
	pub format: AudioFormat,
	/// Samples per second per channel.
	pub sample_rate: u32,
	/// Number of channels.
	pub channel_count: u32,
	/// Presentation timestamp of the first frame, in microseconds.
	pub timestamp_us: u64,
	/// Raw PCM bytes.
	pub data: Bytes,
}

impl AudioSamples {
	/// Number of complete frames (one sample per channel) in this buffer.
	pub fn frame_count(&self) -> usize {
		let stride = self.format.bytes_per_sample() * self.channel_count as usize;
		self.data.len().checked_div(stride).unwrap_or(0)
	}

	/// Duration of these samples in microseconds, based on `sample_rate`.
	pub fn duration_us(&self) -> u64 {
		(self.frame_count() as u64 * 1_000_000)
			.checked_div(self.sample_rate as u64)
			.unwrap_or(0)
	}
}
