//! Hardware H.264 backend via NVIDIA NVENC (`nvidia-video-codec-sdk` + cudarc).
//!
//! Linux only, behind the `nvenc` feature. The NVENC API lives in the driver
//! (`libnvidia-encode.so`) and cudarc loads CUDA dynamically, so this is not a
//! build-time dependency on the CUDA toolkit. NVENC emits Annex-B with in-band
//! SPS/PPS, matching avc3 mode directly.
//!
//! NOT YET VALIDATED ON HARDWARE. Two things need checking on a real Linux+GPU
//! box before this ships in releases:
//!   1. The safe wrapper's input buffer does a flat `write`, which only matches
//!      NVENC's chosen pitch when the width is suitably aligned (multiples of 64
//!      are safe). Non-aligned widths would need pitched writes via the `sys`
//!      lock API. We warn at open if the width looks risky.
//!   2. The exact `NV_ENC_CONFIG` field set for rate control / GOP.

use std::sync::Arc;

use bytes::Bytes;
use cudarc::driver::CudaContext;
use nvidia_video_codec_sdk::sys::nvEncodeAPI::{
	NV_ENC_BUFFER_FORMAT, NV_ENC_CODEC_H264_GUID, NV_ENC_PARAMS_RC_MODE, NV_ENC_PIC_TYPE, NV_ENC_PRESET_P4_GUID,
	NV_ENC_TUNING_INFO,
};
use nvidia_video_codec_sdk::{Encoder, EncoderInitParams, Session};

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::Frame;

pub(crate) const NAME: &str = "nvenc";

pub(crate) struct Nvenc {
	session: Session,
	// Keep the CUDA context alive for as long as the session uses it.
	_cuda: Arc<CudaContext>,
	timestamp: u64,
}

// Used only from the single capture/encode thread (see `publish_capture`).
unsafe impl Send for Nvenc {}

impl Nvenc {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		if config.width % 64 != 0 {
			// Flat writes assume pitch == width; NVENC aligns pitch, so a
			// non-64-aligned width risks corrupting the encoded chroma. Fail so
			// `Kind::Auto` falls back to the next backend instead of producing
			// garbage.
			return Err(Error::Codec(anyhow::anyhow!(
				"nvenc requires a width that is a multiple of 64 (got {})",
				config.width
			)));
		}

		let cuda = CudaContext::new(0).map_err(|e| Error::Codec(anyhow::anyhow!("CUDA init: {e}")))?;
		let encoder = Encoder::initialize_with_cuda(cuda.clone())
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC init: {e}")))?;

		// Start from the low-latency P4 preset, then set bitrate and GOP.
		let mut preset = encoder
			.get_preset_config(
				NV_ENC_CODEC_H264_GUID,
				NV_ENC_PRESET_P4_GUID,
				NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY,
			)
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC preset config: {e}")))?;

		let cfg = &mut preset.presetCfg;
		cfg.gopLength = config.gop;
		cfg.frameIntervalP = 1; // no B-frames
		cfg.rcParams.rateControlMode = NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
		cfg.rcParams.averageBitRate = config.resolved_bitrate().min(u32::MAX as u64) as u32;

		let mut init = EncoderInitParams::new(NV_ENC_CODEC_H264_GUID, config.width, config.height);
		init.preset_guid(NV_ENC_PRESET_P4_GUID)
			.tuning_info(NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY)
			.framerate(config.framerate, 1)
			.enable_picture_type_decision()
			.encode_config(cfg);

		let session = encoder
			.start_session(NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_IYUV, init)
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC start session: {e}")))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Box::new(Self {
			session,
			_cuda: cuda,
			timestamp: 0,
		}))
	}
}

impl Backend for Nvenc {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		let mut input = self
			.session
			.create_input_buffer()
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC input buffer: {e}")))?;
		let mut output = self
			.session
			.create_output_bitstream()
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC output bitstream: {e}")))?;

		// NVENC takes CPU I420; download a surface if capture handed us one.
		let i420 = frame.to_i420()?;

		// SAFETY: the lock is held until the guard drops, and we write exactly
		// one I420 frame's worth of bytes. See the pitch caveat at the top.
		unsafe {
			input
				.lock()
				.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC lock input: {e}")))?
				.write(&i420.data);
		}

		let params = nvidia_video_codec_sdk::EncodePictureParams {
			input_timestamp: self.timestamp,
			picture_type: if keyframe {
				NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_IDR
			} else {
				NV_ENC_PIC_TYPE::NV_ENC_PIC_TYPE_UNKNOWN
			},
			..Default::default()
		};
		self.timestamp += 1;

		self.session
			.encode_picture(&mut input, &mut output, params)
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC encode: {e}")))?;

		let data = output
			.lock()
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC lock output: {e}")))?
			.data()
			.to_vec();

		Ok(if data.is_empty() {
			Vec::new()
		} else {
			vec![Bytes::from(data)]
		})
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		// Each encode locks its own output synchronously, so nothing is buffered.
		Ok(Vec::new())
	}

	fn name(&self) -> &str {
		NAME
	}
}
