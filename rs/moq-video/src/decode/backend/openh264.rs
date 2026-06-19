//! Software H.264 decode backend via Cisco's openh264 (vendored, statically linked).
//!
//! The portable fallback when no hardware decoder is available, and the only
//! backend on Linux/Windows for now. Accepts Annex-B access units (SPS/PPS
//! inline ahead of each keyframe) and returns packed I420.

use bytes::Bytes;
use openh264::OpenH264API;
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;

use super::Backend;
use crate::Error;
use crate::frame::I420;

pub(crate) const NAME: &str = "openh264";

pub(crate) struct Openh264 {
	decoder: Decoder,
}

impl Openh264 {
	pub(crate) fn open() -> Result<Box<dyn Backend>, Error> {
		let decoder = Decoder::with_api_config(OpenH264API::from_source(), DecoderConfig::new())
			.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 decoder init: {e}")))?;

		tracing::info!(decoder = NAME, "opened H.264 decoder");
		Ok(Box::new(Self { decoder }))
	}
}

impl Backend for Openh264 {
	fn decode(&mut self, access_unit: Bytes, _keyframe: bool) -> Result<Vec<I420>, Error> {
		let decoded = self
			.decoder
			.decode(&access_unit)
			.map_err(|e| Error::Codec(anyhow::anyhow!("openh264 decode: {e}")))?;

		// `None` means the decoder buffered the access unit but has no picture
		// yet (e.g. parameter sets only, or it needs more data).
		let Some(yuv) = decoded else {
			return Ok(Vec::new());
		};

		let (width, height) = yuv.dimensions();
		if width % 2 != 0 || height % 2 != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"decoded frame has odd dimensions {width}x{height}, expected 4:2:0"
			)));
		}
		let (y_stride, uv_stride, _) = yuv.strides();

		let frame = I420::from_planes(
			yuv.y(),
			yuv.u(),
			yuv.v(),
			y_stride,
			uv_stride,
			width as u32,
			height as u32,
		);
		Ok(vec![frame])
	}

	fn name(&self) -> &str {
		NAME
	}
}
