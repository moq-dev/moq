//! Video frame decoding using FFmpeg.

use super::{DecodeError, DecodedFrame, Decoder, Result};
use crate::{catalog::video::VideoCodec, Frame};
use ffmpeg_next as ffmpeg;
use std::sync::Arc;

/// Raw video frame data after decoding.
#[derive(Debug, Clone)]
pub struct VideoFrame {
	/// Presentation timestamp in microseconds.
	pub timestamp: moq_lite::Timestamp,

	/// Pixel format (YUV420p, YUV422p, RGB24, etc.).
	pub format: VideoFormat,

	/// Frame width in pixels.
	pub width: u32,

	/// Frame height in pixels.
	pub height: u32,

	/// Pixel data organized as planes (e.g., Y, U, V for YUV formats).
	pub planes: Vec<Plane>,
}

/// Video pixel format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
	/// YUV 4:2:0 planar (most common for video codecs)
	YUV420P,
	/// YUV 4:2:2 planar
	YUV422P,
	/// YUV 4:4:4 planar
	YUV444P,
	/// RGB24 packed
	RGB24,
	/// RGBA packed
	RGBA,
}

impl VideoFormat {
	/// Convert from FFmpeg pixel format.
	fn from_ffmpeg(format: ffmpeg::format::Pixel) -> Option<Self> {
		use ffmpeg::format::Pixel;
		match format {
			Pixel::YUV420P => Some(Self::YUV420P),
			Pixel::YUV422P => Some(Self::YUV422P),
			Pixel::YUV444P => Some(Self::YUV444P),
			Pixel::RGB24 => Some(Self::RGB24),
			Pixel::RGBA => Some(Self::RGBA),
			_ => None,
		}
	}
}

/// A single plane of pixel data.
#[derive(Debug, Clone)]
pub struct Plane {
	/// Raw pixel data for this plane.
	pub data: Arc<Vec<u8>>,

	/// Number of bytes between rows (may include padding).
	pub stride: usize,
}

/// Video decoder using FFmpeg.
///
/// Decodes compressed video frames (H.264, H.265, VP9, AV1) to raw YUV/RGB data.
pub struct VideoDecoder {
	decoder: ffmpeg::decoder::Video,
	codec: VideoCodec,
}

impl VideoDecoder {
	/// Create a new video decoder for the given codec.
	///
	/// # Parameters
	///
	/// * `codec` - The video codec to decode
	/// * `extra_data` - Optional codec-specific initialization data (e.g., SPS/PPS for H.264)
	pub fn new(codec: VideoCodec, extra_data: Option<&[u8]>) -> Result<Self> {
		// Initialize FFmpeg (idempotent)
		ffmpeg::init().map_err(|e| DecodeError::InitError(e.to_string()))?;

		// Map our VideoCodec enum to FFmpeg codec ID
		let codec_id = match codec {
			VideoCodec::H264(_) => ffmpeg::codec::Id::H264,
			VideoCodec::H265(_) => ffmpeg::codec::Id::HEVC,
			VideoCodec::VP8 => ffmpeg::codec::Id::VP8,
			VideoCodec::VP9(_) => ffmpeg::codec::Id::VP9,
			VideoCodec::AV1(_) => ffmpeg::codec::Id::AV1,
		};

		// Find the decoder
		let codec = ffmpeg::codec::decoder::find(codec_id)
			.ok_or_else(|| DecodeError::UnsupportedCodec(format!("{:?}", codec_id)))?;

		// Create decoder context
		let mut context = ffmpeg::codec::context::Context::new_with_codec(codec);
		let mut decoder = context.decoder();
		let decoder = decoder
			.video()
			.map_err(|e| DecodeError::InitError(format!("not a video codec: {}", e)))?;

		// Set extra data if provided (e.g., SPS/PPS for H.264)
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

impl Decoder for VideoDecoder {
	fn decode(&mut self, frame: &Frame) -> Result<DecodedFrame> {
		// Create FFmpeg packet from frame data
		let mut packet = ffmpeg::codec::packet::Packet::copy(frame.payload.as_ref());

		// Set packet timestamp (convert microseconds to FFmpeg timebase)
		packet.set_pts(Some(frame.timestamp.0 as i64));

		// Send packet to decoder
		self.decoder
			.send_packet(&packet)
			.map_err(|e| DecodeError::DecodeError(format!("send_packet failed: {}", e)))?;

		// Receive decoded frame
		let mut decoded = ffmpeg::frame::Video::empty();
		self.decoder
			.receive_frame(&mut decoded)
			.map_err(|e| DecodeError::DecodeError(format!("receive_frame failed: {}", e)))?;

		// Convert to our VideoFrame type
		let format = VideoFormat::from_ffmpeg(decoded.format())
			.ok_or_else(|| DecodeError::UnsupportedCodec(format!("pixel format {:?}", decoded.format())))?;

		let width = decoded.width();
		let height = decoded.height();
		let timestamp = frame.timestamp;

		// Extract plane data
		let planes = match format {
			VideoFormat::YUV420P | VideoFormat::YUV422P | VideoFormat::YUV444P => {
				// YUV formats have 3 planes: Y, U, V
				(0..3)
					.map(|i| {
						let data = decoded.data(i).to_vec();
						let stride = decoded.stride(i);
						Plane {
							data: Arc::new(data),
							stride,
						}
					})
					.collect()
			}
			VideoFormat::RGB24 | VideoFormat::RGBA => {
				// Packed formats have 1 plane
				vec![Plane {
					data: Arc::new(decoded.data(0).to_vec()),
					stride: decoded.stride(0),
				}]
			}
		};

		Ok(DecodedFrame::Video(VideoFrame {
			timestamp,
			format,
			width,
			height,
			planes,
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
			let mut decoded = ffmpeg::frame::Video::empty();
			match self.decoder.receive_frame(&mut decoded) {
				Ok(()) => {
					// Convert to VideoFrame (similar to decode())
					let format = VideoFormat::from_ffmpeg(decoded.format())
						.ok_or_else(|| DecodeError::UnsupportedCodec(format!("pixel format {:?}", decoded.format())))?;

					let planes = match format {
						VideoFormat::YUV420P | VideoFormat::YUV422P | VideoFormat::YUV444P => (0..3)
							.map(|i| Plane {
								data: Arc::new(decoded.data(i).to_vec()),
								stride: decoded.stride(i),
							})
							.collect(),
						VideoFormat::RGB24 | VideoFormat::RGBA => {
							vec![Plane {
								data: Arc::new(decoded.data(0).to_vec()),
								stride: decoded.stride(0),
							}]
						}
					};

					frames.push(DecodedFrame::Video(VideoFrame {
						timestamp: moq_lite::Timestamp(decoded.pts().unwrap_or(0) as u64),
						format,
						width: decoded.width(),
						height: decoded.height(),
						planes,
					}));
				}
				Err(_) => break, // No more frames
			}
		}

		Ok(frames)
	}
}
