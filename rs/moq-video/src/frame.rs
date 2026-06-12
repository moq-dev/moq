//! The frame handed from capture to an encoder backend.
//!
//! Two representations so the common path stays zero-copy:
//! - [`Frame::Surface`] is a platform GPU surface (a macOS `CVPixelBuffer`,
//!   IOSurface-backed). The capture source produces it and a hardware encoder
//!   (VideoToolbox) consumes it directly, no copy and no color conversion.
//! - [`Frame::I420`] is CPU-resident planar I420, for the software path
//!   (openh264) and platforms without a zero-copy capture yet.
//!
//! A hardware backend takes a surface as-is; a software backend asks for I420
//! via [`Frame::to_i420`], which downloads the surface only when necessary.

use std::borrow::Cow;

use yuv::{YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, YuvRange, YuvStandardMatrix, rgba_to_yuv420};

use crate::Error;

pub(crate) enum Frame {
	/// Zero-copy GPU surface (macOS `CVPixelBuffer`).
	#[cfg(target_os = "macos")]
	Surface(macos::Surface),
	/// Zero-copy Linux dmabuf (imported as a VA surface by the VAAPI backend).
	#[cfg(target_os = "linux")]
	DmaBuf(linux::DmaSurface),
	/// CPU-resident planar I420.
	I420(I420),
}

impl Frame {
	pub(crate) fn width(&self) -> u32 {
		match self {
			#[cfg(target_os = "macos")]
			Frame::Surface(s) => s.width,
			#[cfg(target_os = "linux")]
			Frame::DmaBuf(s) => s.width,
			Frame::I420(i) => i.width,
		}
	}

	pub(crate) fn height(&self) -> u32 {
		match self {
			#[cfg(target_os = "macos")]
			Frame::Surface(s) => s.height,
			#[cfg(target_os = "linux")]
			Frame::DmaBuf(s) => s.height,
			Frame::I420(i) => i.height,
		}
	}

	/// A CPU I420 view, downloading a GPU surface only if necessary.
	pub(crate) fn to_i420(&self) -> Result<Cow<'_, I420>, Error> {
		match self {
			#[cfg(target_os = "macos")]
			Frame::Surface(s) => Ok(Cow::Owned(s.download_i420()?)),
			#[cfg(target_os = "linux")]
			Frame::DmaBuf(_) => Err(Error::Codec(anyhow::anyhow!(
				"software encoding from a dmabuf is not supported; use VAAPI or a CPU capture"
			))),
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
	/// limited range (studio swing, what H.264 decoders expect by default).
	pub(crate) fn from_rgba(rgba: &[u8], width: u32, height: u32) -> Self {
		let mut planar = YuvPlanarImageMut::alloc(width, height, YuvChromaSubsampling::Yuv420);
		rgba_to_yuv420(
			&mut planar,
			rgba,
			width * 4,
			YuvRange::Limited,
			YuvStandardMatrix::Bt601,
			YuvConversionMode::Balanced,
		)
		.expect("rgba_to_yuv420 with validated dimensions");

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

#[cfg(target_os = "linux")]
pub(crate) mod linux {
	use std::fs::File;

	/// A captured Linux dmabuf, sized to the encoder resolution. The VAAPI
	/// backend imports the fd(s) as a VA surface (DrmPrime2), so capture -> encode
	/// stays zero-copy. Owns its fd(s); a V4L2 capture hands over dup'd handles so
	/// the underlying buffer can be re-queued independently.
	pub(crate) struct DmaSurface {
		/// One fd for a single-buffer (packed) layout, or one per plane.
		pub(crate) fds: Vec<File>,
		pub(crate) width: u32,
		pub(crate) height: u32,
		/// DRM fourcc of the surface, e.g. `*b"NV12"`.
		pub(crate) fourcc: [u8; 4],
		/// DRM format modifier (0 == `DRM_FORMAT_MOD_LINEAR`).
		pub(crate) modifier: u64,
		pub(crate) planes: Vec<DmaPlane>,
	}

	/// One plane's position within the dmabuf(s).
	pub(crate) struct DmaPlane {
		pub(crate) buffer_index: usize,
		pub(crate) offset: usize,
		pub(crate) stride: usize,
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

	impl Surface {
		pub(crate) fn new(buffer: CFRetained<CVPixelBuffer>, width: u32, height: u32) -> Self {
			Self { buffer, width, height }
		}

		/// Download an NV12 surface to packed I420 (the software-encode fallback).
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
