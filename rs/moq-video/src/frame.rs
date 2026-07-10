//! The frame handed from capture to an encoder backend.
//!
//! Representations chosen so the common path stays zero-copy:
//! - [`Frame::Surface`] is a macOS `CVPixelBuffer` (IOSurface-backed NV12). The
//!   capture source produces it and VideoToolbox consumes it directly, no copy
//!   and no color conversion.
//! - [`Frame::Texture`] is a Windows Direct3D11 NV12 texture. Media Foundation
//!   capture produces it on a shared D3D11 device and the hardware encoder MFT
//!   consumes it on that same device, also zero-copy.
//! - [`Frame::I420`] is CPU-resident planar I420, for the CPU encode path and
//!   platforms without a zero-copy capture.
//!
//! A backend that consumes a GPU surface takes the frame as-is; a CPU encoder
//! asks for I420 via [`Frame::to_i420`], which downloads the GPU frame only when
//! needed.

use std::borrow::Cow;

use yuv::{YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix, rgba_to_yuv420};

use crate::Error;

pub(crate) enum Frame {
	/// Zero-copy GPU surface (macOS `CVPixelBuffer`).
	#[cfg(target_os = "macos")]
	Surface(macos::Surface),
	/// Zero-copy GPU texture (Windows Direct3D11 NV12).
	#[cfg(target_os = "windows")]
	Texture(d3d11::Texture),
	/// Zero-copy GPU buffer (Linux CUDA NV12). Produced only by the NVDEC
	/// decoder, consumed in place by the NVENC encoder.
	#[cfg(all(target_os = "linux", feature = "nvdec"))]
	Cuda(cuda::Frame),
	/// CPU-resident planar I420.
	I420(I420),
}

impl Frame {
	pub(crate) fn width(&self) -> u32 {
		match self {
			#[cfg(target_os = "macos")]
			Frame::Surface(s) => s.width,
			#[cfg(target_os = "windows")]
			Frame::Texture(t) => t.width,
			#[cfg(all(target_os = "linux", feature = "nvdec"))]
			Frame::Cuda(c) => c.width,
			Frame::I420(i) => i.width,
		}
	}

	pub(crate) fn height(&self) -> u32 {
		match self {
			#[cfg(target_os = "macos")]
			Frame::Surface(s) => s.height,
			#[cfg(target_os = "windows")]
			Frame::Texture(t) => t.height,
			#[cfg(all(target_os = "linux", feature = "nvdec"))]
			Frame::Cuda(c) => c.height,
			Frame::I420(i) => i.height,
		}
	}

	/// A CPU I420 view, downloading a GPU frame only if necessary.
	pub(crate) fn to_i420(&self) -> Result<Cow<'_, I420>, Error> {
		match self {
			#[cfg(target_os = "macos")]
			Frame::Surface(s) => Ok(Cow::Owned(s.download_i420()?)),
			#[cfg(target_os = "windows")]
			Frame::Texture(t) => Ok(Cow::Owned(t.download_i420()?)),
			#[cfg(all(target_os = "linux", feature = "nvdec"))]
			Frame::Cuda(c) => Ok(Cow::Owned(c.download_i420()?)),
			Frame::I420(i) => Ok(Cow::Borrowed(i)),
		}
	}
}

/// A raw video frame in planar I420 (YUV 4:2:0), tightly packed (no padding),
/// at the encoder resolution. Width and height are even (chroma is 2x2).
#[derive(Clone)]
pub(crate) struct I420 {
	pub width: u32,
	pub height: u32,
	/// Y plane (`width * height`) then U then V (`width/2 * height/2` each).
	pub data: Vec<u8>,
}

impl I420 {
	/// Tightly-packed I420 byte length for the given even dimensions.
	pub(crate) fn len(width: u32, height: u32) -> usize {
		let luma = width as usize * height as usize;
		luma + luma / 2
	}

	/// Convert tightly-packed RGBA (`width * height * 4` bytes) to I420, BT.601
	/// limited range (studio swing, what H.264 decoders expect by default). Used
	/// by [`Encoder::encode_rgba`](crate::encode::Encoder) and the Windows capture.
	pub(crate) fn from_rgba(rgba: &[u8], width: u32, height: u32) -> Result<Self, Error> {
		let mut planar = YuvPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
		rgba_to_yuv420(
			&mut planar,
			rgba,
			width * 4,
			YuvRange::Limited,
			YuvStandardMatrix::Bt601,
			YuvConversionMode::Balanced,
		)
		.map_err(|e| Error::Codec(anyhow::anyhow!("rgba_to_yuv420 failed for {width}x{height}: {e}")))?;
		Ok(Self::pack(&planar, width, height))
	}

	/// Convert BGRA to I420, BT.601 limited range. `stride` is the source row
	/// pitch in bytes (>= `width * 4`), so a padded surface maps directly. Used by
	/// the Windows Desktop Duplication screen-capture path, whose staging texture
	/// is BGRA with a driver-chosen row pitch.
	#[cfg(target_os = "windows")]
	pub(crate) fn from_bgra(bgra: &[u8], stride: u32, width: u32, height: u32) -> Result<Self, Error> {
		use yuv::bgra_to_yuv420;

		let mut planar = YuvPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
		bgra_to_yuv420(
			&mut planar,
			bgra,
			stride,
			YuvRange::Limited,
			YuvStandardMatrix::Bt601,
			YuvConversionMode::Balanced,
		)
		.map_err(|e| Error::Codec(anyhow::anyhow!("bgra_to_yuv420 failed for {width}x{height}: {e}")))?;
		Ok(Self::pack(&planar, width, height))
	}

	/// Pack strided Y/U/V planes (4:2:0, full-size luma, half-size chroma) into a
	/// tightly-packed I420 buffer. `y_stride` / `uv_stride` are the source row
	/// strides, which a decoder may pad wider than the visible width. Used by the
	/// software H.264 decode backend, whose `DecodedYUV` exposes strided planes.
	/// Width and height must be even (4:2:0 chroma).
	pub(crate) fn from_planes(
		y: &[u8],
		u: &[u8],
		v: &[u8],
		y_stride: usize,
		uv_stride: usize,
		width: u32,
		height: u32,
	) -> Self {
		let (w, h) = (width as usize, height as usize);
		let (cw, ch) = (w / 2, h / 2);

		let mut data = vec![0u8; Self::len(width, height)];
		let (luma, chroma) = data.split_at_mut(w * h);
		let (u_dst, v_dst) = chroma.split_at_mut(cw * ch);

		for row in 0..h {
			luma[row * w..row * w + w].copy_from_slice(&y[row * y_stride..row * y_stride + w]);
		}
		for row in 0..ch {
			u_dst[row * cw..row * cw + cw].copy_from_slice(&u[row * uv_stride..row * uv_stride + cw]);
			v_dst[row * cw..row * cw + cw].copy_from_slice(&v[row * uv_stride..row * uv_stride + cw]);
		}

		Self { width, height, data }
	}

	/// Convert tightly-packed RGB (`width * height * 3` bytes) to I420, BT.601
	/// limited range. Used for MJPEG capture (Linux V4L2), which decodes to RGB.
	#[cfg(target_os = "linux")]
	pub(crate) fn from_rgb(rgb: &[u8], width: u32, height: u32) -> Result<Self, Error> {
		use yuv::rgb_to_yuv420;

		let mut planar = YuvPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
		rgb_to_yuv420(
			&mut planar,
			rgb,
			width * 3,
			YuvRange::Limited,
			YuvStandardMatrix::Bt601,
			YuvConversionMode::Balanced,
		)
		.map_err(|e| Error::Codec(anyhow::anyhow!("rgb_to_yuv420 failed for {width}x{height}: {e}")))?;
		Ok(Self::pack(&planar, width, height))
	}

	/// Convert packed YUYV (YUV 4:2:2, `stride` bytes per row) to I420. A chroma
	/// resample (4:2:2 -> 4:2:0), no color-space conversion. Used for the raw
	/// V4L2 capture path (Linux).
	#[cfg(target_os = "linux")]
	pub(crate) fn from_yuyv(yuyv: &[u8], stride: u32, width: u32, height: u32) -> Result<Self, Error> {
		use yuv::{YuvPackedImage, yuyv422_to_yuv420};

		let mut planar = YuvPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
		let packed = YuvPackedImage {
			yuy: yuyv,
			yuy_stride: stride,
			width,
			height,
		};
		yuyv422_to_yuv420(&mut planar, &packed)
			.map_err(|e| Error::Codec(anyhow::anyhow!("yuyv422_to_yuv420 failed for {width}x{height}: {e}")))?;
		Ok(Self::pack(&planar, width, height))
	}

	/// Split tightly-packed NV12 (Y plane `width * height`, then interleaved UV
	/// `width/2 * height/2` pairs) into planar I420. A chroma deinterleave, no
	/// color-space conversion. Used for the Windows Media Foundation capture path,
	/// whose source reader hands us NV12.
	#[cfg(target_os = "windows")]
	pub(crate) fn from_nv12(nv12: &[u8], width: u32, height: u32) -> Result<Self, Error> {
		let (w, h) = (width as usize, height as usize);
		let luma = w * h;
		let chroma = luma / 4;
		let need = luma + 2 * chroma;
		if nv12.len() < need {
			return Err(Error::Codec(anyhow::anyhow!(
				"NV12 buffer too small: {} < {need} for {width}x{height}",
				nv12.len()
			)));
		}

		let mut data = vec![0u8; Self::len(width, height)];
		data[..luma].copy_from_slice(&nv12[..luma]);
		let (u_dst, v_dst) = data[luma..].split_at_mut(chroma);
		deinterleave_uv(&nv12[luma..need], u_dst, v_dst);
		Ok(Self { width, height, data })
	}

	/// Flatten the three planes of a freshly-converted image into one tightly
	/// packed I420 buffer (Y, then U, then V).
	fn pack(planar: &YuvPlanarImageMut<u8>, width: u32, height: u32) -> Self {
		let mut data = Vec::with_capacity(Self::len(width, height));
		data.extend_from_slice(planar.y_plane.borrow());
		data.extend_from_slice(planar.u_plane.borrow());
		data.extend_from_slice(planar.v_plane.borrow());
		Self { width, height, data }
	}

	fn luma_len(&self) -> usize {
		self.width as usize * self.height as usize
	}

	fn chroma_len(&self) -> usize {
		self.luma_len() / 4
	}

	pub(crate) fn y(&self) -> &[u8] {
		&self.data[..self.luma_len()]
	}

	pub(crate) fn u(&self) -> &[u8] {
		let start = self.luma_len();
		&self.data[start..start + self.chroma_len()]
	}

	pub(crate) fn v(&self) -> &[u8] {
		let start = self.luma_len() + self.chroma_len();
		&self.data[start..start + self.chroma_len()]
	}
}

/// Interleave separate U and V planes into a packed NV12 chroma plane
/// (`u[i], v[i]` -> `uv[2i], uv[2i+1]`). `uv` must be twice the length of `u`.
#[cfg(any(target_os = "windows", all(target_os = "linux", feature = "nvenc")))]
pub(crate) fn interleave_uv(u: &[u8], v: &[u8], uv: &mut [u8]) {
	for (pair, (u, v)) in uv.chunks_exact_mut(2).zip(u.iter().zip(v)) {
		pair[0] = *u;
		pair[1] = *v;
	}
}

/// Split a packed NV12 chroma plane into separate U and V planes, the inverse of
/// [`interleave_uv`].
#[cfg(target_os = "windows")]
pub(crate) fn deinterleave_uv(uv: &[u8], u: &mut [u8], v: &mut [u8]) {
	for (pair, (u, v)) in uv.chunks_exact(2).zip(u.iter_mut().zip(v)) {
		*u = pair[0];
		*v = pair[1];
	}
}

#[cfg(target_os = "macos")]
pub(crate) mod macos {
	use std::ptr;

	use objc2_core_foundation::CFRetained;
	use objc2_core_video::{
		CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
		CVPixelBufferGetPixelFormatType, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
		CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
		kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
	};

	use super::I420;
	use crate::Error;

	/// Read-only lock flag (`kCVPixelBufferLock_ReadOnly`).
	const LOCK_READ_ONLY: CVPixelBufferLockFlags = CVPixelBufferLockFlags(1);

	/// A captured GPU surface. Cloning is a cheap retain (no pixel copy), which
	/// is what keeps the capture -> encode path zero-copy.
	pub(crate) struct Surface {
		pub(crate) buffer: CFRetained<CVPixelBuffer>,
		pub(crate) width: u32,
		pub(crate) height: u32,
	}

	// SAFETY: CVPixelBuffer is a reference-counted CoreFoundation wrapper around
	// an IOSurface. Retain/release are thread-safe and the pixel data is only
	// touched under CVPixelBufferLockBaseAddress, so the handle can move between
	// threads (capture delegate -> encode loop, decode task -> caller). objc2
	// leaves CoreVideo types !Send out of conservatism.
	unsafe impl Send for Surface {}

	impl Surface {
		pub(crate) fn new(buffer: CFRetained<CVPixelBuffer>, width: u32, height: u32) -> Self {
			Self { buffer, width, height }
		}

		/// Download an NV12 surface to packed I420 (the CPU encode path).
		pub(crate) fn download_i420(&self) -> Result<I420, Error> {
			let format = CVPixelBufferGetPixelFormatType(&self.buffer);
			if format != kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
				&& format != kCVPixelFormatType_420YpCbCr8BiPlanarFullRange
			{
				return Err(Error::Codec(anyhow::anyhow!(
					"cannot download pixel format {format:#x}; expected NV12"
				)));
			}

			let (w, h) = (self.width as usize, self.height as usize);
			let (cw, ch) = (w / 2, h / 2);

			let status = unsafe { CVPixelBufferLockBaseAddress(&self.buffer, LOCK_READ_ONLY) };
			if status != 0 {
				return Err(Error::Codec(anyhow::anyhow!(
					"CVPixelBufferLockBaseAddress failed: {status}"
				)));
			}
			let _guard = UnlockGuard(&self.buffer);

			let mut data = vec![0u8; I420::len(self.width, self.height)];
			let (luma, chroma) = data.split_at_mut(w * h);
			let (u_plane, v_plane) = chroma.split_at_mut(cw * ch);

			// Plane 0: Y, copied row by row honoring stride.
			let y_base = CVPixelBufferGetBaseAddressOfPlane(&self.buffer, 0) as *const u8;
			let y_stride = CVPixelBufferGetBytesPerRowOfPlane(&self.buffer, 0);
			for row in 0..h {
				unsafe {
					ptr::copy_nonoverlapping(y_base.add(row * y_stride), luma[row * w..].as_mut_ptr(), w);
				}
			}

			// Plane 1: interleaved UV -> split into U and V.
			let uv_base = CVPixelBufferGetBaseAddressOfPlane(&self.buffer, 1) as *const u8;
			let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(&self.buffer, 1);
			for row in 0..ch {
				let src = unsafe { uv_base.add(row * uv_stride) };
				for col in 0..cw {
					unsafe {
						u_plane[row * cw + col] = *src.add(col * 2);
						v_plane[row * cw + col] = *src.add(col * 2 + 1);
					}
				}
			}

			Ok(I420 {
				width: self.width,
				height: self.height,
				data,
			})
		}
	}

	struct UnlockGuard<'a>(&'a CVPixelBuffer);

	impl Drop for UnlockGuard<'_> {
		fn drop(&mut self) {
			unsafe { CVPixelBufferUnlockBaseAddress(self.0, LOCK_READ_ONLY) };
		}
	}
}

#[cfg(all(target_os = "linux", feature = "nvdec"))]
pub(crate) mod cuda {
	use std::sync::Arc;

	use cudarc::driver::{CudaContext, result};

	use super::I420;
	use crate::Error;

	/// An owned device allocation. Plain `cuMemAlloc` on purpose: NVENC's
	/// resource registration rejects stream-ordered pool memory
	/// (`cuMemAllocAsync`), which is what cudarc's `CudaSlice` uses on any GPU
	/// with memory-pool support.
	struct Buffer {
		ctx: Arc<CudaContext>,
		ptr: cudarc::driver::sys::CUdeviceptr,
		len: usize,
	}

	impl Drop for Buffer {
		fn drop(&mut self) {
			// Drop may run on any thread; freeing needs the context current.
			if self.ctx.bind_to_thread().is_ok() {
				// SAFETY: the pointer came from `malloc_sync` and is freed once.
				let _ = unsafe { result::free_sync(self.ptr) };
			}
		}
	}

	/// A GPU NV12 frame in CUDA device memory: NVDEC's output and NVENC's
	/// zero-copy input. One buffer holds both planes at a shared row `pitch`:
	/// `height` luma rows, then `height / 2` interleaved-UV rows. Cloning bumps
	/// refcounts (no pixel copy), which keeps decode -> encode on the GPU.
	///
	/// Both codecs use the device's primary CUDA context (`CudaContext::new`
	/// retains it), so a frame decoded by NVDEC is directly addressable by NVENC.
	#[derive(Clone)]
	pub(crate) struct Frame {
		buf: Arc<Buffer>,
		pub(crate) width: u32,
		pub(crate) height: u32,
		/// Row pitch in bytes of both planes (>= `width`).
		pub(crate) pitch: u32,
	}

	impl Frame {
		/// Allocate an NV12 buffer for `width` x `height` (both even) at row
		/// pitch `pitch`. Uninitialized: the caller copies the full extent in.
		pub(crate) fn alloc(ctx: &Arc<CudaContext>, width: u32, height: u32, pitch: u32) -> Result<Self, Error> {
			debug_assert!(pitch >= width && width % 2 == 0 && height % 2 == 0);
			let len = pitch as usize * height as usize * 3 / 2;
			ctx.bind_to_thread()
				.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
			// SAFETY: a plain device allocation; ownership lands in `Buffer`,
			// whose Drop frees it exactly once.
			let ptr = unsafe { result::malloc_sync(len) }
				.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA alloc of {len} bytes: {e:?}")))?;
			Ok(Self {
				buf: Arc::new(Buffer {
					ctx: ctx.clone(),
					ptr,
					len,
				}),
				width,
				height,
				pitch,
			})
		}

		/// The raw device pointer, for FFI (the NVDEC copy destination, the
		/// NVENC resource registration). Valid while `self` is alive.
		pub(crate) fn device_ptr(&self) -> u64 {
			self.buf.ptr
		}

		/// Download and de-pitch to packed I420 (the CPU fallback: a software
		/// encoder, or a caller that wants bytes).
		pub(crate) fn download_i420(&self) -> Result<I420, Error> {
			self.buf
				.ctx
				.bind_to_thread()
				.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA bind: {e:?}")))?;
			let mut host = vec![0u8; self.buf.len];
			// SAFETY: the buffer is `len` bytes of device memory and stays alive
			// for the synchronous copy.
			unsafe { result::memcpy_dtoh_sync(&mut host, self.buf.ptr) }
				.map_err(|e| Error::Codec(anyhow::anyhow!("CUDA download: {e:?}")))?;

			let (w, h) = (self.width as usize, self.height as usize);
			let (cw, ch) = (w / 2, h / 2);
			let pitch = self.pitch as usize;

			let mut data = vec![0u8; I420::len(self.width, self.height)];
			let (luma, chroma) = data.split_at_mut(w * h);
			let (u_dst, v_dst) = chroma.split_at_mut(cw * ch);

			for row in 0..h {
				luma[row * w..row * w + w].copy_from_slice(&host[row * pitch..row * pitch + w]);
			}
			let uv_base = pitch * h;
			for row in 0..ch {
				let src = &host[uv_base + row * pitch..uv_base + row * pitch + w];
				for col in 0..cw {
					u_dst[row * cw + col] = src[col * 2];
					v_dst[row * cw + col] = src[col * 2 + 1];
				}
			}

			Ok(I420 {
				width: self.width,
				height: self.height,
				data,
			})
		}
	}
}

#[cfg(target_os = "windows")]
pub(crate) mod d3d11 {
	use std::ptr;

	use windows::Win32::Foundation::HMODULE;
	use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
	use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
	use windows::Win32::Graphics::Direct3D11::{
		D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_MAP_READ,
		D3D11_MAPPED_SUBRESOURCE, D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, D3D11CreateDevice,
		ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D,
	};
	use windows::core::Interface;

	use super::I420;
	use crate::Error;

	fn err(ctx: &str, e: windows::core::Error) -> Error {
		Error::Codec(anyhow::anyhow!("{ctx}: {e}"))
	}

	/// Create a hardware Direct3D11 device, multithread-protected (Media
	/// Foundation's internal threads or DXGI duplication and our capture thread
	/// both touch it). The shared low-level constructor behind the Media
	/// Foundation device manager and the Desktop Duplication capture path.
	pub(crate) fn create_device() -> Result<ID3D11Device, Error> {
		let mut device: Option<ID3D11Device> = None;
		unsafe {
			D3D11CreateDevice(
				None,
				D3D_DRIVER_TYPE_HARDWARE,
				HMODULE::default(),
				D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
				None,
				D3D11_SDK_VERSION,
				Some(&mut device),
				None,
				None,
			)
			.map_err(|e| err("D3D11CreateDevice", e))?;
		}
		let device = device.ok_or_else(|| Error::Codec(anyhow::anyhow!("D3D11CreateDevice returned null")))?;

		let multithread = device
			.cast::<ID3D10Multithread>()
			.map_err(|e| err("query ID3D10Multithread", e))?;
		unsafe {
			let _ = multithread.SetMultithreadProtected(true);
		}
		Ok(device)
	}

	/// A captured GPU texture (NV12) on the Media Foundation source reader's
	/// Direct3D11 device. Holds the device so the download fallback and the
	/// hardware encoder run on the same device that owns the texture. Cloning the
	/// COM handles is a cheap `AddRef`, which is what keeps capture -> encode
	/// zero-copy.
	pub(crate) struct Texture {
		pub(crate) device: ID3D11Device,
		pub(crate) texture: ID3D11Texture2D,
		/// The texture-array slice this frame lives in. Media Foundation pools the
		/// reader's output as one texture array and reports the index per sample.
		pub(crate) subresource: u32,
		pub(crate) width: u32,
		pub(crate) height: u32,
	}

	impl Texture {
		pub(crate) fn new(
			device: ID3D11Device,
			texture: ID3D11Texture2D,
			subresource: u32,
			width: u32,
			height: u32,
		) -> Self {
			Self {
				device,
				texture,
				subresource,
				width,
				height,
			}
		}

		/// Copy the NV12 texture to a CPU-readable staging texture and
		/// deinterleave it into packed I420 (the CPU encode path, when the encoder
		/// can't consume the GPU texture directly).
		pub(crate) fn download_i420(&self) -> Result<I420, Error> {
			let context = unsafe { self.device.GetImmediateContext() }.map_err(|e| err("GetImmediateContext", e))?;

			// A CPU-readable copy of the source texture's single slice.
			let mut desc = D3D11_TEXTURE2D_DESC::default();
			unsafe { self.texture.GetDesc(&mut desc) };
			desc.ArraySize = 1;
			desc.MipLevels = 1;
			desc.Usage = D3D11_USAGE_STAGING;
			desc.BindFlags = 0;
			desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ.0 as u32;
			desc.MiscFlags = 0;

			let mut staging: Option<ID3D11Texture2D> = None;
			unsafe {
				self.device
					.CreateTexture2D(&desc, None, Some(&mut staging))
					.map_err(|e| err("CreateTexture2D (staging)", e))?;
			}
			let staging = staging.ok_or_else(|| Error::Codec(anyhow::anyhow!("CreateTexture2D returned null")))?;

			unsafe {
				context.CopySubresourceRegion(&staging, 0, 0, 0, 0, &self.texture, self.subresource, None);
			}

			let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
			unsafe {
				context
					.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
					.map_err(|e| err("Map (staging)", e))?;
			}
			let _guard = UnmapGuard {
				context: &context,
				resource: &staging,
			};

			let (w, h) = (self.width as usize, self.height as usize);
			let (cw, ch) = (w / 2, h / 2);
			let pitch = mapped.RowPitch as usize;
			let base = mapped.pData as *const u8;
			// The UV plane begins after the *texture's* Y plane, which spans the
			// allocated height, not the display height. A DXVA decode pool allocates
			// textures at the coded size (e.g. 1088 rows for a 1080p display), so
			// keying the offset off `self.height` would read chroma from inside the
			// still-luma padding rows and produce garbage color.
			let tex_height = desc.Height as usize;

			let mut data = vec![0u8; I420::len(self.width, self.height)];
			let (luma, chroma) = data.split_at_mut(w * h);
			let (u_plane, v_plane) = chroma.split_at_mut(cw * ch);

			// Y plane: h rows of `pitch` bytes, only the first w used.
			for row in 0..h {
				unsafe {
					ptr::copy_nonoverlapping(base.add(row * pitch), luma[row * w..].as_mut_ptr(), w);
				}
			}
			// Interleaved UV plane sits right after the full Y plane, h/2 rows.
			let uv_base = unsafe { base.add(pitch * tex_height) };
			for row in 0..ch {
				let src = unsafe { uv_base.add(row * pitch) };
				for col in 0..cw {
					unsafe {
						u_plane[row * cw + col] = *src.add(col * 2);
						v_plane[row * cw + col] = *src.add(col * 2 + 1);
					}
				}
			}

			Ok(I420 {
				width: self.width,
				height: self.height,
				data,
			})
		}
	}

	struct UnmapGuard<'a> {
		context: &'a ID3D11DeviceContext,
		resource: &'a ID3D11Texture2D,
	}

	impl Drop for UnmapGuard<'_> {
		fn drop(&mut self) {
			unsafe { self.context.Unmap(self.resource, 0) };
		}
	}
}
