//! Dynamically-loaded NVDEC (CUVID) entry points.
//!
//! The decoder API lives in `libnvcuvid` (part of the NVIDIA driver), a separate
//! library from the encoder's `libnvidia-encode`. The raw bindings in
//! [`sys`](crate::sys) declare the functions in an `extern "C"` block, but
//! calling those directly would make the *linker* require `libnvcuvid`, which a
//! GPU-less build machine doesn't have. So, like the encode side, everything is
//! resolved at runtime with dlopen. Unlike the encode side's lazy static (which
//! panics when the driver is absent), [`Api::get`] returns an error so callers
//! can fall back to another decoder.

use core::ffi::{c_int, c_uint, c_ulonglong};
use std::sync::OnceLock;

use cudarc::driver::sys::CUresult;

use crate::sys::cuviddec::{CUvideodecoder, CUVIDDECODECAPS, CUVIDDECODECREATEINFO, CUVIDPICPARAMS, CUVIDPROCPARAMS};
use crate::sys::nvcuvid::{CUvideoparser, CUVIDPARSERPARAMS, CUVIDSOURCEDATAPACKET};

// Function type aliases matching the declarations in `sys`.
type CreateVideoParser = unsafe extern "C" fn(*mut CUvideoparser, *mut CUVIDPARSERPARAMS) -> CUresult;
type ParseVideoData = unsafe extern "C" fn(CUvideoparser, *mut CUVIDSOURCEDATAPACKET) -> CUresult;
type DestroyVideoParser = unsafe extern "C" fn(CUvideoparser) -> CUresult;
type GetDecoderCaps = unsafe extern "C" fn(*mut CUVIDDECODECAPS) -> CUresult;
type CreateDecoder = unsafe extern "C" fn(*mut CUvideodecoder, *mut CUVIDDECODECREATEINFO) -> CUresult;
type DestroyDecoder = unsafe extern "C" fn(CUvideodecoder) -> CUresult;
type DecodePicture = unsafe extern "C" fn(CUvideodecoder, *mut CUVIDPICPARAMS) -> CUresult;
type MapVideoFrame64 =
	unsafe extern "C" fn(CUvideodecoder, c_int, *mut c_ulonglong, *mut c_uint, *mut CUVIDPROCPARAMS) -> CUresult;
type UnmapVideoFrame64 = unsafe extern "C" fn(CUvideodecoder, c_ulonglong) -> CUresult;

/// The NVDEC entry points, resolved from `libnvcuvid` at runtime.
///
/// A caller drives the usual CUVID flow: create a parser
/// ([`create_video_parser`](Self::create_video_parser)), feed it bitstream
/// packets, and inside the parser callbacks create a decoder, decode pictures,
/// and map/unmap the decoded frames. All calls require a current CUDA context.
#[allow(missing_debug_implementations)]
pub struct Api {
	#[doc(alias = "cuvidCreateVideoParser")]
	pub create_video_parser: CreateVideoParser,
	#[doc(alias = "cuvidParseVideoData")]
	pub parse_video_data: ParseVideoData,
	#[doc(alias = "cuvidDestroyVideoParser")]
	pub destroy_video_parser: DestroyVideoParser,
	#[doc(alias = "cuvidGetDecoderCaps")]
	pub get_decoder_caps: GetDecoderCaps,
	#[doc(alias = "cuvidCreateDecoder")]
	pub create_decoder: CreateDecoder,
	#[doc(alias = "cuvidDestroyDecoder")]
	pub destroy_decoder: DestroyDecoder,
	#[doc(alias = "cuvidDecodePicture")]
	pub decode_picture: DecodePicture,
	#[doc(alias = "cuvidMapVideoFrame64")]
	pub map_video_frame: MapVideoFrame64,
	#[doc(alias = "cuvidUnmapVideoFrame64")]
	pub unmap_video_frame: UnmapVideoFrame64,
}

impl Api {
	/// The dlopen'd NVDEC API, or an error when `libnvcuvid` (or one of its
	/// symbols) is unavailable, i.e. the NVIDIA driver is not installed.
	pub fn get() -> Result<&'static Api, &'static str> {
		static API: OnceLock<Result<Api, String>> = OnceLock::new();
		API.get_or_init(Api::load).as_ref().map_err(|e| e.as_str())
	}

	fn load() -> Result<Api, String> {
		// `.so.1` is the versioned SONAME present at runtime; `.so` is the dev
		// symlink. Windows ships the DLL with the NVIDIA driver.
		#[cfg(target_os = "linux")]
		const CANDIDATES: &[&str] = &["libnvcuvid.so.1", "libnvcuvid.so"];
		#[cfg(target_os = "windows")]
		const CANDIDATES: &[&str] = &["nvcuvid.dll"];
		// No NVDEC library exists on other platforms (e.g. macOS); loading
		// always fails there with a clear error.
		#[cfg(not(any(target_os = "linux", target_os = "windows")))]
		const CANDIDATES: &[&str] = &[];

		// SAFETY: loading the NVIDIA driver library runs its initializers, which
		// is sound for driver libs. The handle is leaked so the resolved function
		// pointers stay valid for the process lifetime.
		unsafe {
			let library = CANDIDATES
				.iter()
				.find_map(|name| libloading::Library::new(*name).ok())
				.ok_or_else(|| format!("failed to dlopen the NVIDIA decode library (tried {CANDIDATES:?})"))?;
			let library: &'static libloading::Library = Box::leak(Box::new(library));

			// SAFETY: each symbol is resolved by the exact name and signature the
			// vendored `sys` bindings declare for it.
			unsafe fn sym<T: Copy>(library: &'static libloading::Library, name: &str) -> Result<T, String> {
				let symbol: libloading::Symbol<T> = unsafe { library.get(name.as_bytes()) }.map_err(|e| {
					let name = name.trim_end_matches('\0');
					format!("symbol {name} missing from the NVIDIA decode library: {e}")
				})?;
				Ok(*symbol)
			}

			Ok(Api {
				create_video_parser: sym(library, "cuvidCreateVideoParser\0")?,
				parse_video_data: sym(library, "cuvidParseVideoData\0")?,
				destroy_video_parser: sym(library, "cuvidDestroyVideoParser\0")?,
				get_decoder_caps: sym(library, "cuvidGetDecoderCaps\0")?,
				create_decoder: sym(library, "cuvidCreateDecoder\0")?,
				destroy_decoder: sym(library, "cuvidDestroyDecoder\0")?,
				decode_picture: sym(library, "cuvidDecodePicture\0")?,
				map_video_frame: sym(library, "cuvidMapVideoFrame64\0")?,
				unmap_video_frame: sym(library, "cuvidUnmapVideoFrame64\0")?,
			})
		}
	}
}
