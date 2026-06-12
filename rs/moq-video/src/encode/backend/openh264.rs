//! Software H.264 backend via Cisco's openh264 (vendored, statically linked).
//!
//! The fallback when no hardware encoder is available. Emits Annex-B with
//! in-band SPS/PPS, ready for `moq_mux::codec::h264::Import` in avc3 mode.

use bytes::Bytes;
use openh264::OpenH264API;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, RateControlMode, UsageType};
use openh264::formats::YUVSlices;

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::Frame;

pub(crate) const NAME: &str = "openh264";

pub(crate) struct Openh264 {
	encoder: Encoder,
}

impl Openh264 {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let cfg = EncoderConfig::new()
			.bitrate(BitRate::from_bps(config.resolved_bitrate().min(u32::MAX as u64) as u32))
			.max_frame_rate(FrameRate::from_hz(config.framerate as f32))
			.rate_control_mode(RateControlMode::Bitrate)
			// Real-time camera: prioritize latency over compression.
			.usage_type(UsageType::CameraVideoRealTime)
			.intra_frame_period(IntraFramePeriod::from_num_frames(config.gop));

		let encoder = Encoder::with_api_config(OpenH264API::from_source(), cfg)
			.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 init: {e}")))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Box::new(Self { encoder }))
	}
}

impl Backend for Openh264 {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		if keyframe {
			self.encoder.force_intra_frame();
		}

		// Software path: needs CPU I420, downloading a GPU surface if necessary.
		let i420 = frame.to_i420()?;
		let (w, h) = (i420.width as usize, i420.height as usize);
		let yuv = YUVSlices::new((i420.y(), i420.u(), i420.v()), (w, h), (w, w / 2, w / 2));

		let bitstream = self
			.encoder
			.encode(&yuv)
			.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 encode: {e}")))?;

		// One Annex-B access unit per frame (low-delay, no B-frames). A skipped
		// frame yields an empty bitstream.
		let bytes = bitstream.to_vec();
		Ok(if bytes.is_empty() {
			Vec::new()
		} else {
			vec![Bytes::from(bytes)]
		})
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		// Low-delay: nothing is buffered, so there's nothing to flush.
		Ok(Vec::new())
	}

	fn name(&self) -> &str {
		NAME
	}
}
