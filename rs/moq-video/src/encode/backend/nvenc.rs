//! Hardware H.264 / H.265 backend via NVIDIA NVENC (`moq-nvenc` + cudarc).
//!
//! Linux only, always-on (cfg-gated). The NVENC API lives in the driver
//! (`libnvidia-encode.so`) and cudarc loads CUDA dynamically, so this is not a
//! build-time dependency on the CUDA toolkit. NVENC emits Annex-B with in-band
//! parameter sets (SPS/PPS for H.264, VPS/SPS/PPS for H.265), matching the
//! inline avc3 / hev1 mode directly. The codec is chosen by [`Config::codec`];
//! only the codec GUID differs, the preset / GOP / rate-control setup is shared.
//!
//! Three hardware details this backend gets right (all verified on a Linux +
//! NVIDIA box, see the tests below):
//!   1. A forced keyframe uses the `FORCEIDR` picture flag, not `pictureType`.
//!      Picture-type decision stays on (the low-latency presets are tuned for
//!      it), which makes NVENC ignore `pictureType`; `FORCEIDR` still applies and
//!      is how [`Nvenc::encode`] turns `keyframe` into an out-of-cadence IDR.
//!   2. `repeatSPSPPS` is set so every IDR (not just the first) carries in-band
//!      SPS/PPS (plus VPS for HEVC), which a mid-stream subscriber's avc3 / hev1
//!      importer needs to start decoding at any keyframe.
//!   3. The input frame is copied row by row at NVENC's chosen buffer pitch. The
//!      pitch is aligned (e.g. 512 for a 320-wide buffer) and usually exceeds the
//!      width, so a flat copy would shear the image; [`Nvenc::encode`] writes
//!      each plane pitched, which works for any (even) width.
//!
//! The session's input format is NV12 (NVENC's native layout). A CPU I420 frame
//! is interleaved into an NVENC input buffer; a CUDA frame ([`Frame::Cuda`],
//! NVDEC output, already NV12) is registered as an external resource and encoded
//! in place, so the NVDEC -> NVENC transcode path never touches the CPU.

use std::ffi::c_void;
use std::sync::Arc;

use bytes::Bytes;
use cudarc::driver::CudaContext;
use moq_nvenc::sys::nvEncodeAPI::{
	GUID, NV_ENC_BUFFER_FORMAT, NV_ENC_CODEC_H264_GUID, NV_ENC_CODEC_HEVC_GUID, NV_ENC_INPUT_RESOURCE_TYPE,
	NV_ENC_PARAMS_RC_MODE, NV_ENC_PRESET_P4_GUID, NV_ENC_TUNING_INFO,
};
use moq_nvenc::{Encoder, EncoderInitParams, Session};

use super::super::encoder::{Codec, Config};
use super::Backend;
use crate::Error;
use crate::frame::{Frame, interleave_uv};

pub(crate) const NAME: &str = "nvenc";

/// The NVENC codec GUID for a requested [`Codec`]. The presets, GOP, and rate
/// control are codec-agnostic, so this is the only codec-dependent input.
fn codec_guid(codec: Codec) -> GUID {
	match codec {
		Codec::H264 => NV_ENC_CODEC_H264_GUID,
		Codec::H265 => NV_ENC_CODEC_HEVC_GUID,
	}
}

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
		// cudarc and the NVENC SDK dlopen their driver libraries lazily and
		// *panic* (which aborts the process, since release builds set
		// `panic = "abort"`) when a library is missing, e.g. on a host with no
		// NVIDIA driver. With hardware encoders always-on, `Kind::Auto` (the
		// default) hits this on every GPU-less Linux box, so probe the libraries
		// up front and return an error to fall back to the next encoder.
		if !driver_libs_present() {
			return Err(Error::Codec(anyhow::anyhow!(
				"NVIDIA driver libraries not found (libcuda / libnvidia-encode); NVENC unavailable"
			)));
		}

		// cudarc 0.19's DriverError is Debug-only (no Display), so format with `{e:?}`.
		let codec_guid = codec_guid(config.codec);

		let cuda = CudaContext::new(0).map_err(|e| Error::Codec(anyhow::anyhow!("CUDA init: {e:?}")))?;
		let encoder = Encoder::initialize_with_cuda(cuda.clone())
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC init: {e}")))?;

		// Start from the low-latency P4 preset, then set bitrate and GOP.
		let mut preset = encoder
			.get_preset_config(
				codec_guid,
				NV_ENC_PRESET_P4_GUID,
				NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY,
			)
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC preset config: {e}")))?;

		let cfg = &mut preset.presetCfg;
		cfg.gopLength = config.gop;
		cfg.frameIntervalP = 1; // no B-frames
		cfg.rcParams.rateControlMode = NV_ENC_PARAMS_RC_MODE::NV_ENC_PARAMS_RC_CBR;
		cfg.rcParams.averageBitRate = config.resolved_bitrate().min(u32::MAX as u64) as u32;

		// Two codec-specific knobs the importer relies on, verified on hardware:
		//   - repeatSPSPPS: emit the parameter sets in-band ahead of *every* IDR,
		//     not just the first. A moq subscriber can join at any keyframe and the
		//     avc3 / hev1 importer reads SPS/PPS (plus VPS for HEVC) from each one;
		//     without this NVENC sends them once and later keyframes are
		//     undecodable for late joiners.
		//   - idrPeriod == gopLength: make every periodic I-frame an IDR (a clean
		//     random-access point), so each GOP boundary is joinable.
		//
		// SAFETY: the preset config's codec union was initialized by
		// `get_preset_config` for `codec_guid`, so we write the matching arm.
		unsafe {
			match config.codec {
				Codec::H264 => {
					cfg.encodeCodecConfig.h264Config.set_repeatSPSPPS(1);
					cfg.encodeCodecConfig.h264Config.idrPeriod = config.gop;
				}
				Codec::H265 => {
					cfg.encodeCodecConfig.hevcConfig.set_repeatSPSPPS(1);
					cfg.encodeCodecConfig.hevcConfig.idrPeriod = config.gop;
				}
			}
		}

		let mut init = EncoderInitParams::new(codec_guid, config.width, config.height);
		// Picture-type decision on: NVENC owns the P/IDR structure and inserts an
		// IDR every `gopLength`. The low-latency presets are tuned for this mode;
		// driving picture types by hand (PTD off) misbehaves on these presets.
		init.preset_guid(NV_ENC_PRESET_P4_GUID)
			.tuning_info(NV_ENC_TUNING_INFO::NV_ENC_TUNING_INFO_LOW_LATENCY)
			.framerate(config.framerate, 1)
			.enable_picture_type_decision()
			.encode_config(cfg);

		// NV12 is NVENC's native input layout and what NVDEC emits, so a CUDA
		// frame registers directly; the CPU path interleaves I420 chroma on write.
		let session = encoder
			.start_session(NV_ENC_BUFFER_FORMAT::NV_ENC_BUFFER_FORMAT_NV12, init)
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC start session: {e}")))?;

		tracing::info!(
			encoder = NAME,
			codec = ?config.codec,
			width = config.width,
			height = config.height,
			"opened encoder"
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
		let mut output = self
			.session
			.create_output_bitstream()
			.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC output bitstream: {e}")))?;

		let params = moq_nvenc::EncodePictureParams {
			input_timestamp: self.timestamp,
			force_idr: keyframe,
			..Default::default()
		};
		self.timestamp += 1;

		match frame {
			// A CUDA frame is already NV12 in device memory (NVDEC output):
			// register its buffer as an external NVENC resource and encode in
			// place, no CPU round trip and no GPU copy.
			Frame::Cuda(cuda) => {
				// Registration keeps a raw pointer into the frame; the frame
				// outlives it (`resource` drops inside this arm, unregistering).
				let mut resource = self
					.session
					.register_generic_resource(
						(),
						NV_ENC_INPUT_RESOURCE_TYPE::NV_ENC_INPUT_RESOURCE_TYPE_CUDADEVICEPTR,
						cuda.device_ptr() as *mut c_void,
						cuda.pitch,
					)
					.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC register CUDA frame: {e}")))?;

				self.session
					.encode_picture(&mut resource, &mut output, params)
					.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC encode: {e}")))?;
			}
			// Everything else goes through a CPU NV12 input buffer.
			frame => {
				let mut input = self
					.session
					.create_input_buffer()
					.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC input buffer: {e}")))?;

				let i420 = frame.to_i420()?;

				// Interleave the I420 chroma planes into NV12's single UV plane.
				let (w, h) = (i420.width as usize, i420.height as usize);
				let mut uv = vec![0u8; w * h / 2];
				interleave_uv(i420.u(), i420.v(), &mut uv);

				// Write both planes honoring NVENC's chosen row stride: the
				// buffer pitch can exceed the width even for aligned widths, so a
				// flat write would shear the image. NV12 chroma rows are full
				// width (interleaved U+V) at the luma pitch.
				let mut lock = input
					.lock()
					.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC lock input: {e}")))?;
				let pitch = lock.pitch() as usize;
				// SAFETY: offsets stay within the pitch*height*3/2 NV12 buffer and
				// each source plane is exactly row_bytes * rows.
				unsafe {
					lock.write_rows(0, pitch, i420.y(), w, h);
					lock.write_rows(pitch * h, pitch, &uv, w, h / 2);
				}
				drop(lock);

				self.session
					.encode_picture(&mut input, &mut output, params)
					.map_err(|e| Error::Codec(anyhow::anyhow!("NVENC encode: {e}")))?;
			}
		}

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

/// Whether both NVIDIA driver libraries NVENC needs can be dlopen'd: libcuda
/// (used by cudarc) and libnvidia-encode (the NVENC API). Each crate loads its
/// library lazily and panics if it's absent, so we probe the same names here
/// first and turn a missing driver into a recoverable `Err`.
fn driver_libs_present() -> bool {
	// libcuda is the CUDA driver API; matches cudarc's "cuda" search.
	const CUDA: &[&str] = &["libcuda.so.1", "libcuda.so"];
	// Matches the NVENC SDK's own dynamic-loading candidate list.
	const NVENC: &[&str] = &["libnvidia-encode.so.1", "libnvidia-encode.so"];

	// SAFETY: we only open the library to test presence and immediately drop the
	// handle; we never call into it. Loading runs the library's initializers,
	// which is sound for these driver libs.
	let loadable = |names: &[&str]| {
		names
			.iter()
			.any(|name| unsafe { libloading::Library::new(*name) }.is_ok())
	};
	loadable(CUDA) && loadable(NVENC)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::encode::Config;

	/// On a host without the NVIDIA driver, opening NVENC must return an `Err`
	/// (so `Kind::Auto` falls back) rather than panicking in cudarc / the NVENC
	/// SDK loader. On a box that does have the driver this is a no-op.
	#[test]
	fn missing_driver_errors_instead_of_panicking() {
		if driver_libs_present() {
			return; // real driver present: open() would legitimately try to run
		}
		let config = Config::new(1920, 1080, 30);
		assert!(Nvenc::open(&config).is_err());
	}

	/// A mid-gray RGBA frame, encodable without a camera.
	fn gray_rgba(width: u32, height: u32) -> Vec<u8> {
		vec![0x80u8; width as usize * height as usize * 4]
	}

	/// H.264 NAL unit types in an Annex-B buffer, found via 3-byte start codes (a
	/// 4-byte `00 00 00 01` code contains `00 00 01` too, so this catches both).
	fn h264_nal_types(annexb: &[u8]) -> Vec<u8> {
		let mut types = Vec::new();
		let mut i = 0;
		while i + 3 < annexb.len() {
			if annexb[i..i + 3] == [0, 0, 1] {
				types.push(annexb[i + 3] & 0x1f);
				i += 3;
			} else {
				i += 1;
			}
		}
		types
	}

	/// HEVC NAL unit types in an Annex-B buffer (type = `(byte >> 1) & 0x3f`).
	fn hevc_nal_types(annexb: &[u8]) -> Vec<u8> {
		let mut types = Vec::new();
		let mut i = 0;
		while i + 3 < annexb.len() {
			if annexb[i..i + 3] == [0, 0, 1] {
				types.push((annexb[i + 3] >> 1) & 0x3f);
				i += 3;
			} else {
				i += 1;
			}
		}
		types
	}

	/// Real-hardware H.264 encode through NVENC. Skipped (returns) on a box with
	/// no NVIDIA driver, so it is a no-op on GPU-less CI and validates on a real
	/// Linux + NVIDIA box. Asserts a self-contained IDR (inline SPS+PPS+slice)
	/// and, critically, that a mid-stream *forced* keyframe repeats the parameter
	/// sets: the avc3 importer relies on every IDR carrying its own SPS/PPS, which
	/// NVENC only does with `repeatSPSPPS` enabled.
	#[test]
	fn nvenc_h264_keyframes_carry_param_sets() {
		if !driver_libs_present() {
			return;
		}
		let config = crate::encode::Config {
			kind: crate::encode::Kind::Named(NAME.into()),
			..crate::encode::Config::new(320, 240, 30)
		};
		let Ok(mut encoder) = crate::encode::Encoder::new(&config) else {
			// Driver present but NVENC still unusable (e.g. GPU busy); don't fail.
			return;
		};
		assert_eq!(encoder.name(), NAME);

		let frame = gray_rgba(320, 240);
		// Force a keyframe on frame 0 and again mid-stream at frame 5.
		let mut first = Vec::new();
		let mut forced = Vec::new();
		for i in 0..10u32 {
			let keyframe = i == 0 || i == 5;
			let packets = encoder.encode_rgba(&frame, 320, 240, keyframe).unwrap();
			let joined: Vec<u8> = packets.iter().flatten().copied().collect();
			if i == 0 {
				first = joined;
			} else if i == 5 {
				forced = joined;
			}
		}

		let types = h264_nal_types(&first);
		assert!(types.contains(&7), "no SPS in first IDR: {types:?}");
		assert!(types.contains(&8), "no PPS in first IDR: {types:?}");
		assert!(types.contains(&5), "first packet is not an IDR: {types:?}");

		let types = h264_nal_types(&forced);
		assert!(types.contains(&5), "forced keyframe is not an IDR: {types:?}");
		assert!(types.contains(&7), "forced IDR is missing inline SPS: {types:?}");
		assert!(types.contains(&8), "forced IDR is missing inline PPS: {types:?}");
	}

	/// Real-hardware H.265 encode through NVENC. Same skip rule as the H.264 test.
	/// Asserts the HEVC GUID path emits Annex-B with a self-contained IRAP
	/// (VPS+SPS+PPS+IDR slice) and repeats them on a mid-stream forced keyframe,
	/// which the hev1 importer relies on.
	#[test]
	fn nvenc_h265_keyframes_carry_param_sets() {
		if !driver_libs_present() {
			return;
		}
		let config = crate::encode::Config {
			codec: crate::encode::Codec::H265,
			kind: crate::encode::Kind::Named(NAME.into()),
			..crate::encode::Config::new(320, 240, 30)
		};
		let Ok(mut encoder) = crate::encode::Encoder::new(&config) else {
			return;
		};
		assert_eq!(encoder.name(), NAME);
		assert_eq!(encoder.codec(), crate::encode::Codec::H265);

		let frame = gray_rgba(320, 240);
		let mut first = Vec::new();
		let mut forced = Vec::new();
		for i in 0..10u32 {
			let keyframe = i == 0 || i == 5;
			let packets = encoder.encode_rgba(&frame, 320, 240, keyframe).unwrap();
			let joined: Vec<u8> = packets.iter().flatten().copied().collect();
			if i == 0 {
				first = joined;
			} else if i == 5 {
				forced = joined;
			}
		}

		let is_irap = |t: &u8| (16..=23).contains(t);
		let types = hevc_nal_types(&first);
		assert!(types.contains(&32), "no VPS in first IRAP: {types:?}");
		assert!(types.contains(&33), "no SPS in first IRAP: {types:?}");
		assert!(types.contains(&34), "no PPS in first IRAP: {types:?}");
		assert!(types.iter().any(is_irap), "first packet is not an IRAP: {types:?}");

		let types = hevc_nal_types(&forced);
		assert!(types.iter().any(is_irap), "forced keyframe is not an IRAP: {types:?}");
		assert!(types.contains(&32), "forced IRAP is missing inline VPS: {types:?}");
		assert!(types.contains(&33), "forced IRAP is missing inline SPS: {types:?}");
		assert!(types.contains(&34), "forced IRAP is missing inline PPS: {types:?}");
	}

	/// The capture producer forces a keyframe only on the first frame and relies
	/// on the backend to insert periodic IDRs at the GOP boundary. Verify those
	/// happen without a forced keyframe and each carries inline SPS/PPS, so a
	/// mid-stream subscriber can join at any GOP boundary.
	#[test]
	fn nvenc_h264_periodic_idr_at_gop() {
		if !driver_libs_present() {
			return;
		}
		let mut config = crate::encode::Config::new(320, 240, 30);
		config.kind = crate::encode::Kind::Named(NAME.into());
		config.gop = 3;
		let Ok(mut encoder) = crate::encode::Encoder::new(&config) else {
			return;
		};

		let frame = gray_rgba(320, 240);
		let mut idr_frames = Vec::new();
		// Never force a keyframe (only frame 0 would be); the backend must insert
		// IDRs at frames 3 and 6 on its own.
		for i in 0..7u32 {
			let packets = encoder.encode_rgba(&frame, 320, 240, false).unwrap();
			let joined: Vec<u8> = packets.iter().flatten().copied().collect();
			let types = h264_nal_types(&joined);
			if types.contains(&5) {
				assert!(types.contains(&7), "periodic IDR at frame {i} missing SPS: {types:?}");
				assert!(types.contains(&8), "periodic IDR at frame {i} missing PPS: {types:?}");
				idr_frames.push(i);
			}
		}
		assert_eq!(idr_frames, vec![0, 3, 6], "IDRs not at the expected GOP boundaries");
	}

	/// A static RGBA gradient that varies in both axes, so the chroma planes have
	/// spatial structure and an input-pitch bug shears the decoded image.
	fn gradient_rgba(width: u32, height: u32) -> Vec<u8> {
		let (w, h) = (width as usize, height as usize);
		let mut buf = vec![0u8; w * h * 4];
		for y in 0..h {
			for x in 0..w {
				let i = (y * w + x) * 4;
				buf[i] = (x * 255 / w) as u8;
				buf[i + 1] = (y * 255 / h) as u8;
				buf[i + 2] = ((x + y) * 255 / (w + h)) as u8;
				buf[i + 3] = 255;
			}
		}
		buf
	}

	/// End-to-end pitch check with no external decoder: encode a spatial gradient
	/// through NVENC, decode it with the in-crate openh264 software decoder, and
	/// assert the result matches the input. NVENC's input buffer pitch exceeds the
	/// width (e.g. 512 for 320), so a flat copy would shear the image; this guards
	/// the pitched write in [`Nvenc::encode`]. Uses a width that is not a multiple
	/// of 64 so pitch != width is actually exercised.
	#[test]
	fn nvenc_h264_pitched_write_roundtrips() {
		if !driver_libs_present() {
			return;
		}
		let (w, h) = (300u32, 240u32);
		let config = crate::encode::Config {
			kind: crate::encode::Kind::Named(NAME.into()),
			..crate::encode::Config::new(w, h, 30)
		};
		let Ok(mut encoder) = crate::encode::Encoder::new(&config) else {
			return;
		};

		let rgba = gradient_rgba(w, h);
		let expected = crate::frame::I420::from_rgba(&rgba, w, h).unwrap();

		let decode_config = crate::decode::Config {
			kind: crate::decode::Kind::Software,
			..crate::decode::Config::new()
		};
		let mut decoder = crate::decode::backend::open(crate::decode::backend::Codec::H264, &decode_config).unwrap();

		// A static image, so every decoded frame equals the input regardless of
		// decoder latency or frame reordering.
		let mut decoded = None;
		for i in 0..10u64 {
			for packet in encoder.encode_rgba(&rgba, w, h, i == 0).unwrap() {
				for out in decoder.decode(packet, i * 33_333, i == 0).unwrap() {
					decoded = Some(out.frame.to_i420().unwrap().into_owned());
				}
			}
		}
		let decoded = decoded.expect("decoder produced at least one frame");

		// Mean absolute error per plane. A pitch bug shears the chroma and pushes
		// this well above 20; compression alone keeps it near zero.
		let mae =
			|a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() / a.len() as u64;
		assert!(mae(decoded.y(), expected.y()) < 8, "Y plane corrupt (pitch?)");
		assert!(mae(decoded.u(), expected.u()) < 8, "U plane corrupt (pitch?)");
		assert!(mae(decoded.v(), expected.v()) < 8, "V plane corrupt (pitch?)");
	}
}
