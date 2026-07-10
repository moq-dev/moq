//! CPU I420 scaling between decode and re-encode.

use fast_image_resize::images::{Image, ImageRef};
use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer};

use crate::Error;

/// Scales tightly-packed I420 frames to a fixed output resolution.
///
/// Per-plane SIMD resize: Y at full size, U/V at quarter size. The fallback for
/// decoders without a built-in scaler (openh264, VideoToolbox, Media
/// Foundation); NVDEC scales on the GPU during decode instead, so its frames
/// never come through here.
pub(crate) struct Scaler {
	resizer: Resizer,
	options: ResizeOptions,
	width: u32,
	height: u32,
}

impl Scaler {
	/// A scaler producing `width` x `height` output (both even).
	pub fn new(width: u32, height: u32) -> Self {
		Self {
			resizer: Resizer::new(),
			// Bilinear: the cheapest convolution, fine for live downscaling.
			options: ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)),
			width,
			height,
		}
	}

	/// Scale one packed I420 frame (Y then U then V, no row padding, even
	/// dimensions) to the output resolution.
	pub fn scale(&mut self, src: &[u8], src_width: u32, src_height: u32) -> Result<Vec<u8>, Error> {
		let src_luma = src_width as usize * src_height as usize;
		if src.len() < src_luma * 3 / 2 {
			return Err(Error::Scale(format!(
				"I420 buffer is {} bytes, expected {} for {src_width}x{src_height}",
				src.len(),
				src_luma * 3 / 2
			)));
		}

		let dst_luma = self.width as usize * self.height as usize;
		let mut dst = vec![0u8; dst_luma * 3 / 2];
		let (dst_y, dst_chroma) = dst.split_at_mut(dst_luma);
		let (dst_u, dst_v) = dst_chroma.split_at_mut(dst_luma / 4);

		let src_chroma = src_luma / 4;
		let (src_w2, src_h2) = (src_width / 2, src_height / 2);
		let (dst_w2, dst_h2) = (self.width / 2, self.height / 2);

		plane(
			&mut self.resizer,
			&self.options,
			&src[..src_luma],
			src_width,
			src_height,
			dst_y,
			self.width,
			self.height,
		)?;
		plane(
			&mut self.resizer,
			&self.options,
			&src[src_luma..src_luma + src_chroma],
			src_w2,
			src_h2,
			dst_u,
			dst_w2,
			dst_h2,
		)?;
		plane(
			&mut self.resizer,
			&self.options,
			&src[src_luma + src_chroma..src_luma + 2 * src_chroma],
			src_w2,
			src_h2,
			dst_v,
			dst_w2,
			dst_h2,
		)?;

		Ok(dst)
	}
}

/// Resize one 8-bit plane.
#[allow(clippy::too_many_arguments)]
fn plane(
	resizer: &mut Resizer,
	options: &ResizeOptions,
	src: &[u8],
	src_width: u32,
	src_height: u32,
	dst: &mut [u8],
	dst_width: u32,
	dst_height: u32,
) -> Result<(), Error> {
	let src = ImageRef::new(src_width, src_height, src, PixelType::U8).map_err(|e| Error::Scale(e.to_string()))?;
	let mut dst =
		Image::from_slice_u8(dst_width, dst_height, dst, PixelType::U8).map_err(|e| Error::Scale(e.to_string()))?;
	resizer
		.resize(&src, &mut dst, options)
		.map_err(|e| Error::Scale(e.to_string()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn downscale_preserves_flat_planes() {
		// A flat frame stays flat through any correct per-plane resize.
		let (w, h) = (320u32, 240u32);
		let luma = (w * h) as usize;
		let mut src = vec![0u8; luma * 3 / 2];
		src[..luma].fill(120);
		src[luma..luma + luma / 4].fill(90);
		src[luma + luma / 4..].fill(200);

		let mut scaler = Scaler::new(160, 120);
		let dst = scaler.scale(&src, w, h).unwrap();
		let dst_luma = 160 * 120;
		assert_eq!(dst.len(), dst_luma * 3 / 2);
		assert!(dst[..dst_luma].iter().all(|&b| b == 120));
		assert!(dst[dst_luma..dst_luma + dst_luma / 4].iter().all(|&b| b == 90));
		assert!(dst[dst_luma + dst_luma / 4..].iter().all(|&b| b == 200));
	}

	#[test]
	fn rejects_short_buffer() {
		let mut scaler = Scaler::new(160, 120);
		assert!(matches!(scaler.scale(&[0u8; 16], 320, 240), Err(Error::Scale(_))));
	}
}
