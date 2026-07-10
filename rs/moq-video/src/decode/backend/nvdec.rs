//! Hardware H.264 / H.265 decode via NVIDIA NVDEC (`moq-nvenc`'s cuvid table).
//!
//! Linux only, behind the default-on `nvdec` feature. Everything is dlopen'd at
//! runtime (cudarc loads libcuda, the cuvid table loads libnvcuvid), so the
//! binary links on a GPU-less builder and a driverless host falls back to the
//! next decoder (see [`backend::open`](super::open)).
//!
//! Decoded frames come back as NV12 in CUDA device memory ([`Frame::Cuda`]).
//! Each mapped cuvid surface is copied device-to-device into an owned buffer
//! (surfaces come from a small fixed pool, so holding them across calls would
//! stall the decoder), which the NVENC encode backend then registers directly:
//! the decode -> scale -> encode transcode path never touches the CPU. Scaling
//! rides the decoder itself: [`Config::resize`] maps to cuvid's target size, so
//! the hardware emits frames already at the output resolution.
//!
//! The cuvid parser is driven synchronously: each access unit is pushed with
//! `CUVID_PKT_ENDOFPICTURE` and zero display delay, so its callbacks (sequence /
//! decode / display) all fire inside `cuvidParseVideoData` on the calling
//! thread, and the display queue is drained before `decode` returns. Timestamps
//! are threaded through the parser (`ulClockRate` is set to microseconds), so
//! output frames keep correct presentation times even across reordering.

use core::ffi::{c_int, c_uint, c_ulong, c_ulonglong, c_void};
use std::ptr;
use std::sync::Arc;

use bytes::Bytes;
use cudarc::driver::CudaContext;
use moq_nvenc::cuvid;
use moq_nvenc::sys::cuviddec::{
	CUVIDDECODECAPS, CUVIDDECODECREATEINFO, CUVIDPICPARAMS, CUVIDPROCPARAMS, CUvideodecoder, cudaVideoChromaFormat,
	cudaVideoCodec, cudaVideoCreateFlags, cudaVideoDeinterlaceMode, cudaVideoSurfaceFormat,
};
use moq_nvenc::sys::nvcuvid::{
	CUVIDEOFORMAT, CUVIDPARSERDISPINFO, CUVIDPARSERPARAMS, CUVIDSOURCEDATAPACKET, CUvideopacketflags, CUvideoparser,
};

use super::{Backend, Codec, Decoded};
use crate::Error;
use crate::frame::{Frame, cuda};

pub(crate) const NAME: &str = "nvdec";

fn codec_err(msg: String) -> Error {
	Error::Codec(anyhow::anyhow!(msg))
}

pub(crate) struct Nvdec {
	parser: CUvideoparser,
	/// Boxed so its address is stable: the parser holds a raw pointer to it for
	/// the lifetime of the parser (callbacks dereference it during parse).
	state: Box<State>,
}

// Used from one thread at a time (the decode loop); the CUDA context is rebound
// to the current thread on every call.
unsafe impl Send for Nvdec {}

/// State shared with the parser's C callbacks via the user-data pointer.
struct State {
	api: &'static cuvid::Api,
	ctx: Arc<CudaContext>,
	/// Requested output size; `None` decodes at the stream's display size.
	resize: Option<(u32, u32)>,
	decoder: Option<Decoder>,
	/// Pictures the display callback queued, in presentation order. Drained
	/// (mapped and copied out) after each parse call.
	ready: Vec<CUVIDPARSERDISPINFO>,
	/// First error hit inside a callback; callbacks can only return an int, so
	/// the error is surfaced by `decode` after the parse call.
	error: Option<String>,
}

/// An open cuvid decoder plus the output geometry it was created for.
struct Decoder {
	api: &'static cuvid::Api,
	handle: CUvideodecoder,
	/// The coded size it was created for, to detect reconfigures.
	coded: (u32, u32),
	/// The display crop (left, top, right, bottom) it was created for; a crop
	/// change alone also requires a new decoder.
	display_area: (i32, i32, i32, i32),
	/// Output size: cuvid scales to this while writing the output surface.
	width: u32,
	height: u32,
}

impl Drop for Decoder {
	fn drop(&mut self) {
		// SAFETY: the handle is valid and no frame is mapped (every map is
		// paired with an unmap before decode returns). The caller keeps the CUDA
		// context bound.
		unsafe { (self.api.destroy_decoder)(self.handle) };
	}
}

impl Nvdec {
	pub(crate) fn open(codec: Codec, config: &super::Config) -> Result<Box<dyn Backend>, Error> {
		// cudarc panics (aborting under release `panic = "abort"`) when libcuda
		// is missing, so probe it and turn a missing driver into a recoverable
		// error; the cuvid table already loads fallibly.
		if !driver_libs_present() {
			return Err(codec_err(
				"NVIDIA driver libraries not found (libcuda); NVDEC unavailable".into(),
			));
		}
		let api = cuvid::Api::get().map_err(|e| codec_err(format!("NVDEC unavailable: {e}")))?;

		if let Some((w, h)) = config.resize {
			// NV12 output: chroma is 2x2 subsampled, so the target must be even.
			if w == 0 || h == 0 || w % 2 != 0 || h % 2 != 0 {
				return Err(codec_err(format!("NVDEC resize {w}x{h} must be even and non-zero")));
			}
		}

		let cuda_codec = match codec {
			Codec::H264 => cudaVideoCodec::cudaVideoCodec_H264,
			Codec::H265 => cudaVideoCodec::cudaVideoCodec_HEVC,
		};

		// `CudaContext::new` retains the device's primary context, the same one
		// the NVENC backend uses, so frames pass between them without a copy.
		let ctx = CudaContext::new(0).map_err(|e| codec_err(format!("CUDA init: {e:?}")))?;

		let mut state = Box::new(State {
			api,
			ctx,
			resize: config.resize,
			decoder: None,
			ready: Vec::new(),
			error: None,
		});

		let mut params = CUVIDPARSERPARAMS {
			CodecType: cuda_codec,
			// Placeholder until the sequence callback reports the real minimum.
			ulMaxNumDecodeSurfaces: 1,
			// Packet timestamps are in microseconds (default is a 10 MHz clock).
			ulClockRate: 1_000_000,
			// Emit each picture as soon as it decodes (live path, no lookahead).
			ulMaxDisplayDelay: 0,
			pUserData: &mut *state as *mut State as *mut c_void,
			pfnSequenceCallback: Some(sequence_callback),
			pfnDecodePicture: Some(decode_callback),
			pfnDisplayPicture: Some(display_callback),
			..Default::default()
		};

		let mut parser: CUvideoparser = ptr::null_mut();
		// SAFETY: params points at a fully-initialized struct and outlives the
		// call; the user-data pointer stays valid because `state` is boxed and
		// owned by the returned backend alongside the parser.
		let result = unsafe { (api.create_video_parser)(&mut parser, &mut params) };
		if result != cudarc::driver::sys::CUresult::CUDA_SUCCESS {
			return Err(codec_err(format!("cuvidCreateVideoParser: {result:?}")));
		}

		tracing::info!(decoder = NAME, codec = ?codec, resize = ?config.resize, "opened video decoder");
		Ok(Box::new(Self { parser, state }))
	}
}

impl Backend for Nvdec {
	fn decode(&mut self, access_unit: Bytes, timestamp_us: u64, _keyframe: bool) -> Result<Vec<Decoded>, Error> {
		// The parser callbacks (decoder create, decode) and the map/copy below
		// all need the CUDA context current on this thread.
		self.state
			.ctx
			.bind_to_thread()
			.map_err(|e| codec_err(format!("CUDA bind: {e:?}")))?;

		let mut packet = CUVIDSOURCEDATAPACKET {
			// ENDOFPICTURE: each payload is one complete access unit, so the
			// parser emits it immediately instead of waiting for the next AU to
			// detect the picture boundary (one-in one-out latency).
			flags: (CUvideopacketflags::CUVID_PKT_TIMESTAMP as c_ulong)
				| (CUvideopacketflags::CUVID_PKT_ENDOFPICTURE as c_ulong),
			payload_size: access_unit.len() as c_ulong,
			payload: access_unit.as_ptr(),
			timestamp: timestamp_us as i64,
		};

		// SAFETY: parser and packet are valid; the payload outlives the call
		// (the parser copies what it needs before returning).
		let result = unsafe { (self.state.api.parse_video_data)(self.parser, &mut packet) };
		if let Some(error) = self.state.error.take() {
			return Err(codec_err(error));
		}
		if result != cudarc::driver::sys::CUresult::CUDA_SUCCESS {
			return Err(codec_err(format!("cuvidParseVideoData: {result:?}")));
		}

		let ready = std::mem::take(&mut self.state.ready);
		ready.iter().map(|disp| self.state.map_frame(disp)).collect()
	}

	fn name(&self) -> &str {
		NAME
	}
}

impl Drop for Nvdec {
	fn drop(&mut self) {
		// Drop may run on a different thread than decode; the destroy calls
		// (parser here, decoder via `state`) need the context current.
		let _ = self.state.ctx.bind_to_thread();
		// SAFETY: the parser is valid and no parse call is in flight (&mut self).
		unsafe { (self.state.api.destroy_video_parser)(self.parser) };
	}
}

impl State {
	/// Sequence callback body: (re)create the decoder for the reported format.
	/// Returns the decode-surface count the parser should allocate for.
	fn on_sequence(&mut self, format: &CUVIDEOFORMAT) -> Result<c_int, String> {
		if format.chroma_format != cudaVideoChromaFormat::cudaVideoChromaFormat_420 {
			return Err(format!("unsupported chroma format {:?}", format.chroma_format));
		}
		if format.bit_depth_luma_minus8 != 0 || format.bit_depth_chroma_minus8 != 0 {
			return Err(format!(
				"unsupported bit depth {} (only 8-bit is supported)",
				format.bit_depth_luma_minus8 + 8
			));
		}

		let mut caps = CUVIDDECODECAPS {
			eCodecType: format.codec,
			eChromaFormat: format.chroma_format,
			nBitDepthMinus8: 0,
			..Default::default()
		};
		// SAFETY: caps is fully initialized and the CUDA context is current (the
		// callback runs inside `decode`, which binds it).
		let result = unsafe { (self.api.get_decoder_caps)(&mut caps) };
		if result != cudarc::driver::sys::CUresult::CUDA_SUCCESS {
			return Err(format!("cuvidGetDecoderCaps: {result:?}"));
		}
		if caps.bIsSupported == 0 {
			return Err(format!("this GPU cannot decode {:?}", format.codec));
		}
		if caps.nOutputFormatMask & (1 << cudaVideoSurfaceFormat::cudaVideoSurfaceFormat_NV12 as u16) == 0 {
			return Err("this GPU cannot output NV12".into());
		}
		if format.coded_width > caps.nMaxWidth || format.coded_height > caps.nMaxHeight {
			return Err(format!(
				"{}x{} exceeds this GPU's {}x{} decode limit",
				format.coded_width, format.coded_height, caps.nMaxWidth, caps.nMaxHeight
			));
		}

		// Output size: the requested resize, or the display area (the coded size
		// minus cropping). Rounded down to even for NV12.
		let display = (
			(format.display_area.right - format.display_area.left).max(2) as u32 & !1,
			(format.display_area.bottom - format.display_area.top).max(2) as u32 & !1,
		);
		let (width, height) = self.resize.unwrap_or(display);
		let coded = (format.coded_width, format.coded_height);
		let display_area = (
			format.display_area.left,
			format.display_area.top,
			format.display_area.right,
			format.display_area.bottom,
		);

		let surfaces = c_int::from(format.min_num_decode_surfaces.max(1));

		// A repeated sequence header with unchanged geometry (common at every
		// keyframe) keeps the existing decoder. The display crop matters even at
		// the same coded and target sizes: it selects the source rect the scaler
		// reads from.
		if let Some(decoder) = &self.decoder {
			if decoder.coded == coded
				&& decoder.display_area == display_area
				&& (decoder.width, decoder.height) == (width, height)
			{
				return Ok(surfaces);
			}
		}
		self.decoder = None;

		let mut info = CUVIDDECODECREATEINFO {
			ulWidth: format.coded_width as c_ulong,
			ulHeight: format.coded_height as c_ulong,
			ulNumDecodeSurfaces: surfaces as c_ulong,
			CodecType: format.codec,
			ChromaFormat: format.chroma_format,
			ulCreationFlags: cudaVideoCreateFlags::cudaVideoCreate_PreferCUVID as c_ulong,
			bitDepthMinus8: 0,
			// We recreate on geometry changes instead of reconfiguring, so no
			// headroom beyond the current coded size is needed.
			ulMaxWidth: format.coded_width as c_ulong,
			ulMaxHeight: format.coded_height as c_ulong,
			OutputFormat: cudaVideoSurfaceFormat::cudaVideoSurfaceFormat_NV12,
			// moq sources are progressive; Weave passes frames through untouched.
			DeinterlaceMode: cudaVideoDeinterlaceMode::cudaVideoDeinterlaceMode_Weave,
			ulTargetWidth: width as c_ulong,
			ulTargetHeight: height as c_ulong,
			// Each output surface is copied out and unmapped before the next map,
			// so 2 is plenty (1 in flight + 1 being post-processed).
			ulNumOutputSurfaces: 2,
			..Default::default()
		};
		info.display_area.left = format.display_area.left as i16;
		info.display_area.top = format.display_area.top as i16;
		info.display_area.right = format.display_area.right as i16;
		info.display_area.bottom = format.display_area.bottom as i16;
		info.target_rect.right = width as i16;
		info.target_rect.bottom = height as i16;

		let mut handle: CUvideodecoder = ptr::null_mut();
		// SAFETY: info is fully initialized and the CUDA context is current.
		let result = unsafe { (self.api.create_decoder)(&mut handle, &mut info) };
		if result != cudarc::driver::sys::CUresult::CUDA_SUCCESS {
			return Err(format!("cuvidCreateDecoder: {result:?}"));
		}

		tracing::debug!(
			decoder = NAME,
			coded_width = format.coded_width,
			coded_height = format.coded_height,
			width,
			height,
			"created cuvid decoder"
		);
		self.decoder = Some(Decoder {
			api: self.api,
			handle,
			coded,
			display_area,
			width,
			height,
		});
		Ok(surfaces)
	}

	/// Map one decoded picture and copy it device-to-device into an owned CUDA
	/// buffer, so the fixed surface pool is released before the next decode.
	fn map_frame(&self, disp: &CUVIDPARSERDISPINFO) -> Result<Decoded, Error> {
		let decoder = self
			.decoder
			.as_ref()
			.ok_or_else(|| codec_err("display callback fired before a decoder exists".into()))?;

		let mut proc_params = CUVIDPROCPARAMS {
			progressive_frame: disp.progressive_frame,
			top_field_first: disp.top_field_first,
			..Default::default()
		};

		let mut dev_ptr: c_ulonglong = 0;
		let mut pitch: c_uint = 0;
		// SAFETY: the decoder handle is valid, the picture index came from the
		// display callback, and the CUDA context is current.
		let result = unsafe {
			(self.api.map_video_frame)(
				decoder.handle,
				disp.picture_index,
				&mut dev_ptr,
				&mut pitch,
				&mut proc_params,
			)
		};
		if result != cudarc::driver::sys::CUresult::CUDA_SUCCESS {
			return Err(codec_err(format!("cuvidMapVideoFrame: {result:?}")));
		}

		// The mapped surface is NV12 at the decoder's target size: `height` luma
		// rows of `pitch` bytes, then `height / 2` interleaved-UV rows. Copy it
		// linearly into an owned buffer with the identical layout.
		let copied = (|| -> Result<cuda::Frame, Error> {
			let frame = cuda::Frame::alloc(&self.ctx, decoder.width, decoder.height, pitch)?;
			let len = pitch as usize * decoder.height as usize * 3 / 2;
			// SAFETY: both regions are `len` bytes of device memory: the mapped
			// surface by cuvid's layout above, the destination by `alloc`.
			unsafe { cudarc::driver::result::memcpy_dtod_sync(frame.device_ptr(), dev_ptr, len) }
				.map_err(|e| codec_err(format!("CUDA device copy: {e:?}")))?;
			Ok(frame)
		})();

		// SAFETY: unmapping the pointer we just mapped, always, even when the
		// copy failed; a leaked mapping would starve the output-surface pool.
		unsafe { (self.api.unmap_video_frame)(decoder.handle, dev_ptr) };

		Ok(Decoded {
			// Timestamps rode the parser from `decode` (microseconds, unsigned).
			timestamp_us: disp.timestamp.max(0) as u64,
			frame: Frame::Cuda(copied?),
		})
	}
}

/// Sequence callback: a new (or repeated) sequence header. Returns the number of
/// decode surfaces the parser should assume, or 0 on failure.
unsafe extern "C" fn sequence_callback(user: *mut c_void, format: *mut CUVIDEOFORMAT) -> c_int {
	// SAFETY: `user` is the boxed State passed at parser creation, and cuvid
	// invokes callbacks synchronously inside the parse call, so no other
	// reference to it is live.
	let state = unsafe { &mut *(user as *mut State) };
	let format = unsafe { &*format };
	match state.on_sequence(format) {
		Ok(surfaces) => surfaces,
		Err(e) => {
			state.error.get_or_insert(e);
			0
		}
	}
}

/// Decode callback: a picture is ready to be submitted to the hardware.
unsafe extern "C" fn decode_callback(user: *mut c_void, pic: *mut CUVIDPICPARAMS) -> c_int {
	// SAFETY: see `sequence_callback`.
	let state = unsafe { &mut *(user as *mut State) };
	let Some(decoder) = &state.decoder else {
		state
			.error
			.get_or_insert("decode callback fired before a decoder exists".into());
		return 0;
	};
	// SAFETY: the decoder handle is valid and `pic` comes straight from cuvid.
	let result = unsafe { (state.api.decode_picture)(decoder.handle, pic) };
	if result != cudarc::driver::sys::CUresult::CUDA_SUCCESS {
		state.error.get_or_insert(format!("cuvidDecodePicture: {result:?}"));
		return 0;
	}
	1
}

/// Display callback: a decoded picture is ready for output, in presentation
/// order. Queue it; `decode` maps and copies it out after the parse call.
unsafe extern "C" fn display_callback(user: *mut c_void, disp: *mut CUVIDPARSERDISPINFO) -> c_int {
	// SAFETY: see `sequence_callback`.
	let state = unsafe { &mut *(user as *mut State) };
	if disp.is_null() {
		// End-of-stream marker; nothing to queue.
		return 1;
	}
	// SAFETY: non-null `disp` points at a valid dispinfo for this call.
	state.ready.push(unsafe { *disp });
	1
}

/// Whether libcuda can be dlopen'd. cudarc loads it lazily and panics if it's
/// absent, so probe first and turn a missing driver into a recoverable `Err`
/// (libnvcuvid is probed by the cuvid table itself).
fn driver_libs_present() -> bool {
	const CUDA: &[&str] = &["libcuda.so.1", "libcuda.so"];
	// SAFETY: we only open the library to test presence and immediately drop the
	// handle; loading runs the driver lib's initializers, which is sound.
	CUDA.iter()
		.any(|name| unsafe { libloading::Library::new(*name) }.is_ok())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::decode::{Config as DecodeConfig, Kind as DecodeKind};
	use crate::encode::{Codec as EncodeCodec, Config as EncodeConfig, Encoder, Kind as EncodeKind};
	use crate::frame::I420;

	/// Real hardware only: skip (return) on a box without the NVIDIA driver, so
	/// these are no-ops on GPU-less CI and validate on a Linux + NVIDIA box.
	fn hw_available() -> bool {
		driver_libs_present() && cuvid::Api::get().is_ok()
	}

	/// On a host without the NVIDIA driver, opening NVDEC must return an `Err`
	/// (so `Kind::Auto` falls back to openh264) rather than panicking in cudarc
	/// or the cuvid loader. On a box that does have the driver this is a no-op.
	#[test]
	fn missing_driver_errors_instead_of_panicking() {
		if hw_available() {
			return;
		}
		assert!(Nvdec::open(Codec::H264, &decode_config(None)).is_err());
	}

	fn decode_config(resize: Option<(u32, u32)>) -> DecodeConfig {
		DecodeConfig {
			kind: DecodeKind::Named(NAME.into()),
			resize,
			..DecodeConfig::new()
		}
	}

	/// A static RGBA gradient that varies in both axes, so the chroma planes have
	/// spatial structure and a pitch/layout bug corrupts the decoded image.
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

	/// Mean absolute error between two equal-length planes.
	fn mae(a: &[u8], b: &[u8]) -> u64 {
		assert_eq!(a.len(), b.len());
		a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as u64).sum::<u64>() / a.len() as u64
	}

	/// Encode `frames` gradient frames with `encoder` and decode them through
	/// NVDEC, returning the downloaded I420 pictures with their timestamps.
	fn round_trip(mut encoder: Encoder, mut decoder: Box<dyn Backend>, w: u32, h: u32) -> Vec<(u64, I420)> {
		let rgba = gradient_rgba(w, h);
		let mut out = Vec::new();
		for i in 0..10u64 {
			for packet in encoder.encode_rgba(&rgba, w, h, i == 0).unwrap() {
				for decoded in decoder.decode(packet, i * 33_333, i == 0).unwrap() {
					let i420 = decoded.frame.to_i420().unwrap().into_owned();
					out.push((decoded.timestamp_us, i420));
				}
			}
		}
		assert!(!out.is_empty(), "NVDEC produced no frames");
		out
	}

	/// H.264 through the real hardware: openh264 encodes a gradient (Annex-B,
	/// inline SPS/PPS) and NVDEC decodes it. Asserts the downloaded NV12 -> I420
	/// picture matches the input (a layout bug shears a plane and blows the MAE)
	/// and that timestamps ride the parser through 1:1.
	#[test]
	fn nvdec_h264_round_trip() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, 30)
		})
		.unwrap();
		let decoder = Nvdec::open(Codec::H264, &decode_config(None)).expect("NVDEC H.264 decoder");

		let expected = I420::from_rgba(&gradient_rgba(w, h), w, h).unwrap();
		let decoded = round_trip(encoder, decoder, w, h);

		for (i, (timestamp, i420)) in decoded.iter().enumerate() {
			// Zero display delay and no B-frames: one-in one-out, in order.
			assert_eq!(*timestamp, i as u64 * 33_333, "timestamp did not ride the parser");
			assert_eq!((i420.width, i420.height), (w, h));
			assert!(mae(i420.y(), expected.y()) < 8, "Y plane corrupt");
			assert!(mae(i420.u(), expected.u()) < 8, "U plane corrupt");
			assert!(mae(i420.v(), expected.v()) < 8, "V plane corrupt");
		}
	}

	/// The decoder-side scaler: decode 320x240 at a 160x120 target and compare
	/// against a CPU downscale of the input. Loose MAE bound (two different
	/// scaling kernels), but a plane swap or a pitch bug still blows it up.
	#[test]
	fn nvdec_resize_scales_output() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, 30)
		})
		.unwrap();
		let decoder = Nvdec::open(Codec::H264, &decode_config(Some((160, 120)))).expect("NVDEC H.264 decoder");

		// Nearest-neighbor reference downscale of the expected picture.
		let full = I420::from_rgba(&gradient_rgba(w, h), w, h).unwrap();
		let sample = |plane: &[u8], pw: usize, x: usize, y: usize| plane[y * 2 * pw + x * 2];
		let mut expected_y = vec![0u8; 160 * 120];
		for y in 0..120 {
			for x in 0..160 {
				expected_y[y * 160 + x] = sample(full.y(), w as usize, x, y);
			}
		}

		let decoded = round_trip(encoder, decoder, w, h);
		for (_, i420) in &decoded {
			assert_eq!((i420.width, i420.height), (160, 120), "resize was not applied");
			assert!(mae(i420.y(), &expected_y) < 12, "scaled Y plane corrupt");
		}
	}

	/// H.265 has no software encoder, so the HEVC round trip rides NVENC on the
	/// encode side and NVDEC on the decode side.
	#[test]
	fn nvdec_h265_round_trip() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		let Ok(encoder) = Encoder::new(&EncodeConfig {
			codec: EncodeCodec::H265,
			kind: EncodeKind::Named("nvenc".into()),
			..EncodeConfig::new(w, h, 30)
		}) else {
			// Driver present but NVENC unusable (e.g. GPU busy); don't fail.
			return;
		};
		let decoder = Nvdec::open(Codec::H265, &decode_config(None)).expect("NVDEC H.265 decoder");

		let expected = I420::from_rgba(&gradient_rgba(w, h), w, h).unwrap();
		let decoded = round_trip(encoder, decoder, w, h);
		for (_, i420) in &decoded {
			assert_eq!((i420.width, i420.height), (w, h));
			assert!(mae(i420.y(), expected.y()) < 8, "Y plane corrupt");
			assert!(mae(i420.u(), expected.u()) < 8, "U plane corrupt");
			assert!(mae(i420.v(), expected.v()) < 8, "V plane corrupt");
		}
	}

	/// The full zero-copy loop: NVDEC decodes to CUDA frames, NVENC re-encodes
	/// them straight from device memory (the registered-resource path), and a
	/// software decode of the re-encoded stream still matches the original
	/// picture. No CPU pixels exist between NVDEC and NVENC.
	#[test]
	fn nvdec_to_nvenc_zero_copy() {
		if !hw_available() {
			return;
		}
		let (w, h) = (320u32, 240u32);
		// Source stream: software-encoded gradient.
		let source = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(w, h, 30)
		})
		.unwrap();
		// Decode at half size so the hardware scaler is in the loop too.
		let decoder = Nvdec::open(Codec::H264, &decode_config(Some((160, 120)))).expect("NVDEC decoder");

		let mut nvenc = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("nvenc".into()),
			..EncodeConfig::new(160, 120, 30)
		})
		.expect("NVENC encoder");

		// GPU transcode: every decoded frame must be CUDA-resident.
		let decoded = {
			let rgba = gradient_rgba(w, h);
			let mut source = source;
			let mut decoder = decoder;
			let mut frames = Vec::new();
			for i in 0..10u64 {
				for packet in source.encode_rgba(&rgba, w, h, i == 0).unwrap() {
					frames.extend(decoder.decode(packet, i * 33_333, i == 0).unwrap());
				}
			}
			frames
		};
		assert!(!decoded.is_empty());

		let mut packets = Vec::new();
		for (i, out) in decoded.iter().enumerate() {
			assert!(
				matches!(out.frame, Frame::Cuda(_)),
				"NVDEC produced a non-CUDA frame; the zero-copy path is not exercised"
			);
			packets.extend(nvenc.encode_raw(&out.frame, i == 0).unwrap());
		}
		packets.extend(nvenc.finish().unwrap());
		assert!(!packets.is_empty(), "NVENC produced no packets from CUDA frames");

		// Decode the re-encoded stream in software and compare to the source.
		let expected = {
			let full = I420::from_rgba(&gradient_rgba(w, h), w, h).unwrap();
			let mut y = vec![0u8; 160 * 120];
			for row in 0..120 {
				for col in 0..160 {
					y[row * 160 + col] = full.y()[row * 2 * w as usize + col * 2];
				}
			}
			y
		};
		let mut check = crate::decode::backend::open(
			crate::decode::backend::Codec::H264,
			&DecodeConfig {
				kind: DecodeKind::Software,
				..DecodeConfig::new()
			},
		)
		.unwrap();
		let mut last = None;
		for (i, packet) in packets.into_iter().enumerate() {
			for out in check.decode(packet, i as u64, i == 0).unwrap() {
				last = Some(out.frame.to_i420().unwrap().into_owned());
			}
		}
		let last = last.expect("software decoder produced no frames");
		assert_eq!((last.width, last.height), (160, 120));
		assert!(mae(last.y(), &expected) < 16, "transcoded Y plane corrupt");
	}
}
