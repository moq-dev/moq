//! CPU conversion of native video surfaces to packed pixels.
//!
//! [`Surface::to_rgba`](crate::Surface::to_rgba) is the portable rendering exit: it
//! downloads a GPU surface when necessary, applies the surface's color space, and
//! returns owned pixels for an image or UI toolkit.

use yuv::{YuvPlanarImage, yuv420_to_rgba};

use crate::{Color, Error, Size, Surface};

/// CPU surface conversion options.
///
/// `#[non_exhaustive]`: build via [`Config::new`] (or `default()`) and set the
/// fields you care about, so future output options stay additive.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// How to interpret the source's YUV samples, overriding its own metadata.
	///
	/// `None` uses [`Surface::color`] and falls back to [`Color::infer`] when the
	/// decoder or native surface carries no color description.
	pub color: Option<Color>,
}

impl Config {
	/// A default config that honors surface metadata and otherwise infers color.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Owned, tightly packed RGBA8 pixels in row-major order.
#[derive(Clone)]
pub struct Rgba {
	size: Size,
	stride: usize,
	data: Vec<u8>,
}

impl Rgba {
	/// Wrap tightly packed `size` RGBA8 pixels.
	#[cfg(feature = "render")]
	pub(crate) fn new(size: Size, data: Vec<u8>) -> Self {
		debug_assert_eq!(data.len(), size.width as usize * size.height as usize * 4);
		Self {
			size,
			stride: size.width as usize * 4,
			data,
		}
	}

	/// Image dimensions in pixels.
	pub fn size(&self) -> Size {
		self.size
	}

	/// Image width in pixels.
	pub fn width(&self) -> u32 {
		self.size.width
	}

	/// Image height in pixels.
	pub fn height(&self) -> u32 {
		self.size.height
	}

	/// Bytes between adjacent rows, always `width * 4`.
	pub fn stride(&self) -> usize {
		self.stride
	}

	/// Tightly packed RGBA8 pixels.
	pub fn data(&self) -> &[u8] {
		&self.data
	}

	/// Consume the image and return its tightly packed RGBA8 pixels.
	pub fn into_data(self) -> Vec<u8> {
		self.data
	}
}

/// Downloads `surface` if it is not already on the CPU and converts it to RGBA.
///
/// By reference rather than by value: a caller often holds the surface behind
/// an `Arc` it cannot unwrap, most obviously a publisher's preview frame, which
/// is shared with every rendition's encoder and so never has a refcount of one.
/// Nothing here needs to own the surface, and requiring it forced those callers
/// into a full-resolution rescale purely to obtain pixels they already had.
pub(crate) fn rgba(surface: &Surface, config: &Config) -> Result<Rgba, Error> {
	let size = Size::new(surface.width(), surface.height());
	let color = config
		.color
		.or_else(|| surface.color())
		.unwrap_or_else(|| Color::infer(size));
	let i420 = surface.to_i420()?;
	let luma = usize::try_from(size.pixels())
		.map_err(|_| Error::codec(anyhow::anyhow!("RGBA frame {size}: dimensions too large to represent")))?;
	let stride = size.width.checked_mul(4).ok_or_else(|| {
		Error::codec(anyhow::anyhow!(
			"RGBA frame {size}: row stride is too large to represent"
		))
	})?;
	let stride_usize = usize::try_from(stride).map_err(|_| {
		Error::codec(anyhow::anyhow!(
			"RGBA frame {size}: row stride is too large to represent"
		))
	})?;
	let len = stride_usize.checked_mul(size.height as usize).ok_or_else(|| {
		Error::codec(anyhow::anyhow!(
			"RGBA frame {size}: byte length is too large to represent"
		))
	})?;
	let chroma = luma / 4;
	let planar = YuvPlanarImage {
		y_plane: &i420.data[..luma],
		y_stride: size.width,
		u_plane: &i420.data[luma..luma + chroma],
		u_stride: size.width / 2,
		v_plane: &i420.data[luma + chroma..],
		v_stride: size.width / 2,
		width: size.width,
		height: size.height,
	};
	let mut data = vec![0; len];
	let (range, matrix) = color.yuv();
	yuv420_to_rgba(&planar, &mut data, stride, range, matrix)
		.map_err(|e| Error::codec(anyhow::anyhow!("RGBA conversion failed for {size}: {e}")))?;

	Ok(Rgba {
		size,
		stride: stride_usize,
		data,
	})
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::I420;

	/// A resized surface keeps its original matrix even after crossing the
	/// standard-definition boundary. Ignoring that metadata tints saturated
	/// colors while leaving grayscale test images apparently correct.
	#[test]
	fn conversion_uses_the_surface_color() {
		let source_size = Size::new(64, 64);
		let red = [255u8, 0, 0, 255].repeat(source_size.pixels() as usize);
		let source = I420::from_rgba(&red, source_size.width * 4, source_size).unwrap();
		let source = source.resize(Size::new(1280, 720)).unwrap();
		assert_eq!(source.color(), Some(Color::BT601));
		assert_eq!(Color::infer(Size::new(1280, 720)), Color::BT709);

		let image = rgba(&Surface::I420(source), &Config::default()).unwrap();
		let center = (image.height() as usize / 2 * image.stride) + image.width() as usize / 2 * 4;
		let pixel = &image.data[center..center + 4];
		assert!(pixel[0] >= 250, "red channel drifted: {pixel:?}");
		assert!(pixel[1] <= 2 && pixel[2] <= 2, "surface matrix was ignored: {pixel:?}");
		assert_eq!(pixel[3], 255);
	}

	#[test]
	fn conversion_reports_a_tightly_packed_layout() {
		let size = Size::new(64, 32);
		let surface = Surface::I420(I420::new(size, vec![128; I420::len(size).unwrap()]).unwrap());

		let image = rgba(&surface, &Config::default()).unwrap();
		assert_eq!(image.size(), size);
		assert_eq!(image.width(), size.width);
		assert_eq!(image.height(), size.height);
		assert_eq!(image.stride(), size.width as usize * 4);
		assert_eq!(image.data().len(), image.stride() * size.height as usize);
	}
}
