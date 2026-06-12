//! Intel/AMD VAAPI hardware backend via `cros-codecs` (Linux, `vaapi` feature).
//!
//! VAAPI is a *stateless* encode API: the hardware does the slice coding, but the
//! application builds the H.264 bitstream (SPS/PPS, DPB, ref lists). cros-codecs
//! is that bitstream layer; we drive its H.264 VAAPI encoder. Output is an
//! Annex-B elementary stream with in-band SPS/PPS, matching avc3 mode.
//!
//! Zero-copy by design: a captured Linux dmabuf ([`Frame::DmaBuf`]) is imported
//! straight into a VA surface (DrmPrime2), no copy. Links libva (stable soname),
//! which `dlopen`s the GPU driver at runtime.
//!
//! NOT YET VALIDATED. Doesn't compile on the dev machine (Linux + libva only),
//! and needs the V4L2 dmabuf capture (the other half of zero-copy) before it can
//! run end to end. Written against the cros-codecs 0.0.6 API with names checked
//! against source. Needs a Linux + VAAPI box to compile and test.

use std::fs::File;

use bytes::Bytes;
use cros_codecs::encoder::h264::EncoderConfig as H264Config;
use cros_codecs::encoder::stateless::h264::StatelessEncoder;
use cros_codecs::encoder::{FrameMetadata, RateControl, Tunings, VideoEncoder};
use cros_codecs::libva::Display;
use cros_codecs::video_frame::generic_dma_video_frame::GenericDmaVideoFrame;
use cros_codecs::{BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution};

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::Frame;

pub(crate) const NAME: &str = "vaapi";

pub(crate) struct Vaapi {
	encoder: Box<dyn VideoEncoder<GenericDmaVideoFrame>>,
	timestamp: u64,
}

// The encoder and its libva `Display` are `!Send` (`Rc`), but used only from the
// single capture/encode thread (see `publish_capture`).
unsafe impl Send for Vaapi {}

impl Vaapi {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let display = Display::open().ok_or_else(|| Error::Codec(anyhow::anyhow!("open VAAPI display")))?;

		let resolution = Resolution {
			width: config.width,
			height: config.height,
		};
		let h264 = H264Config {
			resolution,
			initial_tunings: Tunings {
				rate_control: RateControl::ConstantBitrate(config.resolved_bitrate()),
				framerate: config.framerate,
				..Default::default()
			},
			..Default::default()
		};

		let encoder = StatelessEncoder::new_vaapi(
			display,
			h264,
			Fourcc::from(b"NV12"),
			resolution,
			false, // low_power
			BlockingMode::Blocking,
		)
		.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI encoder init: {e:?}")))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Box::new(Self {
			encoder: Box::new(encoder),
			timestamp: 0,
		}))
	}

	/// Drain any ready coded bitstream buffers into Annex-B packets.
	fn drain_ready(&mut self) -> Result<Vec<Bytes>, Error> {
		let mut out = Vec::new();
		while let Some(coded) = self
			.encoder
			.poll()
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI poll: {e:?}")))?
		{
			out.push(Bytes::from(coded.bitstream));
		}
		Ok(out)
	}
}

impl Backend for Vaapi {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		let Frame::DmaBuf(surface) = frame else {
			return Err(Error::Codec(anyhow::anyhow!(
				"VAAPI requires a dmabuf capture (Frame::DmaBuf)"
			)));
		};

		let layout = FrameLayout {
			format: (Fourcc::from(&surface.fourcc), surface.modifier),
			size: Resolution {
				width: surface.width,
				height: surface.height,
			},
			planes: surface
				.planes
				.iter()
				.map(|p| PlaneLayout {
					buffer_index: p.buffer_index,
					offset: p.offset,
					stride: p.stride,
				})
				.collect(),
		};

		// Hand cros-codecs its own dup'd fds so the capture can re-queue the
		// underlying V4L2 buffer once this frame is encoded.
		let fds = surface
			.fds
			.iter()
			.map(File::try_clone)
			.collect::<Result<Vec<File>, _>>()
			.map_err(|e| Error::Codec(anyhow::anyhow!("dup dmabuf fd: {e}")))?;

		let dma = GenericDmaVideoFrame::new(fds, layout.clone())
			.map_err(|e| Error::Codec(anyhow::anyhow!("import dmabuf: {e}")))?;

		let meta = FrameMetadata {
			timestamp: self.timestamp,
			layout,
			force_keyframe: keyframe,
		};
		self.timestamp += 1;

		self.encoder
			.encode(meta, dma)
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI encode: {e:?}")))?;
		self.drain_ready()
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		self.encoder
			.drain()
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI drain: {e:?}")))?;
		self.drain_ready()
	}

	fn name(&self) -> &str {
		NAME
	}
}
