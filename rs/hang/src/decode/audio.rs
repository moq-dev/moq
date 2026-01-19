//! Audio frame decoding using FFmpeg.

use super::{DecodeError, DecodedFrame, Decoder, Result};
use crate::{catalog::audio::AudioCodec, Frame};
use ffmpeg_next as ffmpeg;
use std::sync::Arc;

/// Raw audio frame data after decoding.
#[derive(Debug, Clone)]
pub struct AudioFrame {
	/// Presentation timestamp in microseconds.
	pub timestamp: moq_lite::Timestamp,

	/// Sample format (S16, F32, etc.).
	pub format: AudioFormat,

	/// Sample rate in Hz.
	pub sample_rate: u32,

	/// Number of audio channels.
	pub channels: u32,

	/// Raw audio samples.
	pub data: Arc<Vec<u8>>,
}

/// Audio sample format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
	/// Signed 16-bit integer samples (most common)
	S16,
	/// Signed 32-bit integer samples
	S32,
	/// 32-bit floating point samples
	F32,
	/// 64-bit floating point samples
	F64,
}

impl AudioFormat {
	/// Convert from FFmpeg sample format.
	fn from_ffmpeg(format: ffmpeg::format::Sample) -> Option<Self> {
		use ffmpeg::format::Sample;
		match format {
			Sample::I16(_) => Some(Self::S16),
			Sample::I32(_) => Some(Self::S32),
			Sample::F32(_) => Some(Self::F32),
			Sample::F64(_) => Some(Self::F64),
			_ => None,
		}
	}

	/// Get the size of one sample in bytes.
	pub fn sample_size(&self) -> usize {
		match self {
			Self::S16 => 2,
			Self::S32 => 4,
			Self::F32 => 4,
			Self::F64 => 8,
		}
	}
}

/// Audio decoder using FFmpeg.
///
/// Decodes compressed audio frames (AAC, Opus, etc.) to raw PCM data.
pub struct AudioDecoder {
	decoder: ffmpeg::decoder::Audio,
	codec: AudioCodec,
}

impl AudioDecoder {
	/// Create a new audio decoder for the given codec.
	///
	/// # Parameters
	///
	/// * `codec` - The audio codec to decode
	/// * `extra_data` - Optional codec-specific initialization data
	pub fn new(codec: AudioCodec, extra_data: Option<&[u8]>) -> Result<Self> {
		// Initialize FFmpeg (idempotent)
		ffmpeg::init().map_err(|e| DecodeError::InitError(e.to_string()))?;

		// Map our AudioCodec enum to FFmpeg codec ID
		let codec_id = match codec {
			AudioCodec::AAC(_) => ffmpeg::codec::Id::AAC,
			AudioCodec::Opus => ffmpeg::codec::Id::OPUS,
		};

		// Find the decoder
		let codec = ffmpeg::codec::decoder::find(codec_id)
			.ok_or_else(|| DecodeError::UnsupportedCodec(format!("{:?}", codec_id)))?;

		// Create decoder context
		let mut context = ffmpeg::codec::context::Context::new_with_codec(codec);
		let mut decoder = context.decoder();
		let decoder = decoder
			.audio()
			.map_err(|e| DecodeError::InitError(format!("not an audio codec: {}", e)))?;

		// Set extra data if provided
		if let Some(data) = extra_data {
			unsafe {
				let context = decoder.as_mut_ptr();
				(*context).extradata = ffmpeg::sys::av_malloc(data.len()) as *mut u8;
				(*context).extradata_size = data.len() as i32;
				std::ptr::copy_nonoverlapping(data.as_ptr(), (*context).extradata, data.len());
			}
		}

		Ok(Self {
			decoder,
			codec: codec.into(),
		})
	}
}

impl Decoder for AudioDecoder {
	fn decode(&mut self, frame: &Frame) -> Result<DecodedFrame> {
		// Create FFmpeg packet from frame data
		let mut packet = ffmpeg::codec::packet::Packet::copy(frame.payload.as_ref());

		// Set packet timestamp
		packet.set_pts(Some(frame.timestamp.0 as i64));

		// Send packet to decoder
		self.decoder
			.send_packet(&packet)
			.map_err(|e| DecodeError::DecodeError(format!("send_packet failed: {}", e)))?;

		// Receive decoded frame
		let mut decoded = ffmpeg::frame::Audio::empty();
		self.decoder
			.receive_frame(&mut decoded)
			.map_err(|e| DecodeError::DecodeError(format!("receive_frame failed: {}", e)))?;

		// Convert to our AudioFrame type
		let format = AudioFormat::from_ffmpeg(decoded.format())
			.ok_or_else(|| DecodeError::UnsupportedCodec(format!("sample format {:?}", decoded.format())))?;

		let sample_rate = decoded.rate();
		let channels = decoded.channels() as u32;
		let timestamp = frame.timestamp;

		// Extract audio data
		// FFmpeg may use planar format (separate planes per channel) or packed format
		let data = if decoded.is_planar() {
			// Planar format: interleave channels
			let sample_size = format.sample_size();
			let samples_per_channel = decoded.samples();
			let mut interleaved = Vec::with_capacity(samples_per_channel * channels as usize * sample_size);

			for sample_idx in 0..samples_per_channel {
				for channel in 0..channels as usize {
					let plane_data = decoded.data(channel);
					let offset = sample_idx * sample_size;
					interleaved.extend_from_slice(&plane_data[offset..offset + sample_size]);
				}
			}

			interleaved
		} else {
			// Packed format: data is already interleaved
			decoded.data(0).to_vec()
		};

		Ok(DecodedFrame::Audio(AudioFrame {
			timestamp,
			format,
			sample_rate,
			channels,
			data: Arc::new(data),
		}))
	}

	fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
		// Send flush packet
		self.decoder
			.send_eof()
			.map_err(|e| DecodeError::DecodeError(format!("flush failed: {}", e)))?;

		// Receive all buffered frames
		let mut frames = Vec::new();
		loop {
			let mut decoded = ffmpeg::frame::Audio::empty();
			match self.decoder.receive_frame(&mut decoded) {
				Ok(()) => {
					let format = AudioFormat::from_ffmpeg(decoded.format())
						.ok_or_else(|| DecodeError::UnsupportedCodec(format!("sample format {:?}", decoded.format())))?;

					let data = if decoded.is_planar() {
						let sample_size = format.sample_size();
						let samples_per_channel = decoded.samples();
						let channels = decoded.channels() as usize;
						let mut interleaved = Vec::with_capacity(samples_per_channel * channels * sample_size);

						for sample_idx in 0..samples_per_channel {
							for channel in 0..channels {
								let plane_data = decoded.data(channel);
								let offset = sample_idx * sample_size;
								interleaved.extend_from_slice(&plane_data[offset..offset + sample_size]);
							}
						}

						interleaved
					} else {
						decoded.data(0).to_vec()
					};

					frames.push(DecodedFrame::Audio(AudioFrame {
						timestamp: moq_lite::Timestamp(decoded.pts().unwrap_or(0) as u64),
						format,
						sample_rate: decoded.rate(),
						channels: decoded.channels() as u32,
						data: Arc::new(data),
					}));
				}
				Err(_) => break, // No more frames
			}
		}

		Ok(frames)
	}
}
