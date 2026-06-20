//! Intel/AMD VAAPI hardware backend via the `moq-vaapi` crate (Linux, always-on).
//!
//! `moq-vaapi` is a focused VA-API H.264 encoder vendored and trimmed from
//! cros-libva + discord/cros-codecs. It takes tightly-packed NV12 and emits an
//! Annex-B elementary stream with in-band SPS/PPS, matching avc3 mode. libva is
//! dlopen'd at runtime, so this links without libva and the binary loads on
//! machines without it, falling back to software (see [`backend::open`]).
//!
//! Our captures hand us CPU I420 (webcams deliver YUYV/MJPEG, decoded to I420),
//! so each frame is interleaved to NV12 before encoding.
//!
//! NOT YET VALIDATED ON HARDWARE: `moq-vaapi`'s encode path is compile-verified
//! only, so the emitted bitstream needs a Linux + Intel/AMD GPU to confirm at
//! playback.

use bytes::Bytes;
use moq_vaapi::encode::{Config as VaapiConfig, Encoder};

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::{Frame, I420};

pub(crate) const NAME: &str = "vaapi";

pub(crate) struct Vaapi {
	encoder: Encoder,
}

// The encoder is `!Send` (libva uses `Rc` internally) but is only ever touched
// from the single capture/encode thread (see `publish_capture`).
unsafe impl Send for Vaapi {}

impl Vaapi {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let bitrate = config.resolved_bitrate().min(u32::MAX as u64) as u32;
		let vaapi = VaapiConfig::new(config.width, config.height, config.framerate, bitrate, config.gop);
		let encoder = Encoder::new(vaapi).map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI encoder init: {e:?}")))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Box::new(Self { encoder }))
	}
}

impl Backend for Vaapi {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		let i420 = frame.to_i420()?;
		let nv12 = i420_to_nv12(&i420);
		let annexb = self
			.encoder
			.encode_nv12(&nv12, keyframe)
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI encode: {e:?}")))?;

		Ok(if annexb.is_empty() {
			Vec::new()
		} else {
			vec![Bytes::from(annexb)]
		})
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		// The encoder submits and reads back synchronously per frame, so nothing
		// is buffered at shutdown.
		Ok(Vec::new())
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// Interleave tightly-packed I420 into tightly-packed NV12: copy Y as-is, then
/// interleave U and V into the chroma plane.
fn i420_to_nv12(i420: &I420) -> Vec<u8> {
	let (w, h) = (i420.width as usize, i420.height as usize);
	let (cw, ch) = (w / 2, h / 2);

	let mut out = vec![0u8; w * h + 2 * cw * ch];
	out[..w * h].copy_from_slice(i420.y());

	let (u, v) = (i420.u(), i420.v());
	let uv = &mut out[w * h..];
	for i in 0..cw * ch {
		uv[i * 2] = u[i];
		uv[i * 2 + 1] = v[i];
	}
	out
}
