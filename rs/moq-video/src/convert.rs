//! CPU conversion of native video surfaces to packed pixels.
//!
//! [`Surface::to_rgba`](crate::Surface::to_rgba) and
//! [`to_bgra`](crate::Surface::to_bgra) are the portable rendering exits: they
//! download a GPU surface when necessary, apply the surface's color space, and
//! return owned pixels for an image or UI toolkit.
//!
//! Both channel orders are here because toolkits disagree and the conversion is
//! a full pass over the frame. Producing the order the caller wants costs
//! nothing extra; producing the other one and swapping two channels afterwards
//! costs a second pass, which is what a consumer had to write before.

use yuv::{YuvPlanarImage, yuv420_to_bgra, yuv420_to_rgba};

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
	width: u32,
	height: u32,
	stride: usize,
	data: Vec<u8>,
}

impl Rgba {
	/// Image width in pixels.
	pub fn width(&self) -> u32 {
		self.width
	}

	/// Image height in pixels.
	pub fn height(&self) -> u32 {
		self.height
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

/// Owned, tightly packed BGRA8 pixels in row-major order.
///
/// The same bytes as [`Rgba`] with the red and blue channels exchanged. A
/// separate type rather than a flag, so a buffer in one order cannot be handed
/// to something expecting the other.
#[derive(Clone)]
pub struct Bgra {
	width: u32,
	height: u32,
	stride: usize,
	data: Vec<u8>,
}

impl Bgra {
	/// Image width in pixels.
	pub fn width(&self) -> u32 {
		self.width
	}

	/// Image height in pixels.
	pub fn height(&self) -> u32 {
		self.height
	}

	/// Bytes between adjacent rows, always `width * 4`.
	pub fn stride(&self) -> usize {
		self.stride
	}

	/// Tightly packed BGRA8 pixels.
	pub fn data(&self) -> &[u8] {
		&self.data
	}

	/// Consume the image and return its tightly packed BGRA8 pixels.
	pub fn into_data(self) -> Vec<u8> {
		self.data
	}
}

/// The geometry and pixels shared by both channel orders.
struct Packed {
	width: u32,
	height: u32,
	stride: usize,
	data: Vec<u8>,
}

/// One `yuv` entry point, named so a failure can say which.
type Convert =
	fn(&YuvPlanarImage<'_, u8>, &mut [u8], u32, yuv::YuvRange, yuv::YuvStandardMatrix) -> Result<(), yuv::YuvError>;

/// Downloads `surface` if it is not already on the CPU and converts it through
/// `convert`.
///
/// By reference rather than by value: a caller often holds the surface behind
/// an `Arc` it cannot unwrap, most obviously a publisher's preview frame, which
/// is shared with every rendition's encoder and so never has a refcount of one.
/// Nothing here needs to own the surface, and requiring it forced those callers
/// into a full-resolution rescale purely to obtain pixels they already had.
fn packed(surface: &Surface, config: &Config, convert: Convert, name: &str) -> Result<Packed, Error> {
	let size = Size::new(surface.width(), surface.height());
	let color = config
		.color
		.or_else(|| surface.color())
		.unwrap_or_else(|| Color::infer(size));
	let i420 = surface.to_i420()?;
	let luma = usize::try_from(size.pixels()).map_err(|_| {
		Error::Codec(anyhow::anyhow!(
			"{name} frame {size}: dimensions too large to represent"
		))
	})?;
	let stride = size.width.checked_mul(4).ok_or_else(|| {
		Error::Codec(anyhow::anyhow!(
			"{name} frame {size}: row stride is too large to represent"
		))
	})?;
	let stride_usize = usize::try_from(stride).map_err(|_| {
		Error::Codec(anyhow::anyhow!(
			"{name} frame {size}: row stride is too large to represent"
		))
	})?;
	let len = stride_usize.checked_mul(size.height as usize).ok_or_else(|| {
		Error::Codec(anyhow::anyhow!(
			"{name} frame {size}: byte length is too large to represent"
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
	convert(&planar, &mut data, stride, range, matrix)
		.map_err(|e| Error::Codec(anyhow::anyhow!("{name} conversion failed for {size}: {e}")))?;

	Ok(Packed {
		width: size.width,
		height: size.height,
		stride: stride_usize,
		data,
	})
}

pub(crate) fn rgba(surface: &Surface, config: &Config) -> Result<Rgba, Error> {
	let packed = packed(surface, config, yuv420_to_rgba, "RGBA")?;
	Ok(Rgba {
		width: packed.width,
		height: packed.height,
		stride: packed.stride,
		data: packed.data,
	})
}

pub(crate) fn bgra(surface: &Surface, config: &Config) -> Result<Bgra, Error> {
	let packed = packed(surface, config, yuv420_to_bgra, "BGRA")?;
	Ok(Bgra {
		width: packed.width,
		height: packed.height,
		stride: packed.stride,
		data: packed.data,
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
		let source = I420::from_rgba(&red, source_size.width * 4, source_size.width, source_size.height).unwrap();
		let source = source.resize(1280, 720).unwrap();
		assert_eq!(source.color(), Some(Color::Bt601Limited));
		assert_eq!(Color::infer(Size::new(1280, 720)), Color::Bt709Limited);

		let image = rgba(&Surface::I420(source), &Config::default()).unwrap();
		let center = (image.height as usize / 2 * image.stride) + image.width as usize / 2 * 4;
		let pixel = &image.data[center..center + 4];
		assert!(pixel[0] >= 250, "red channel drifted: {pixel:?}");
		assert!(pixel[1] <= 2 && pixel[2] <= 2, "surface matrix was ignored: {pixel:?}");
		assert_eq!(pixel[3], 255);
	}

	#[test]
	fn conversion_reports_a_tightly_packed_layout() {
		let size = Size::new(64, 32);
		let surface = Surface::I420(I420::new(size.width, size.height, vec![128; I420::len(64, 32)]).unwrap());

		let image = rgba(&surface, &Config::default()).unwrap();
		assert_eq!(image.width(), size.width);
		assert_eq!(image.height(), size.height);
		assert_eq!(image.stride(), size.width as usize * 4);
		assert_eq!(image.data().len(), image.stride() * size.height as usize);
	}

	/// The point of shipping BGRA rather than leaving consumers to swap the
	/// channels: the two orders are the same conversion, so they have to agree
	/// pixel for pixel with red and blue exchanged. A consumer doing the swap
	/// itself re-derives the color handling this crate already does, and gets it
	/// wrong for anything that is not a grayscale test pattern.
	#[test]
	fn bgra_is_rgba_with_red_and_blue_exchanged() {
		let size = Size::new(64, 64);
		// Saturated rather than gray, or a transposed matrix would still pass.
		let source = [200u8, 40, 90, 255].repeat(size.pixels() as usize);
		let i420 = I420::from_rgba(&source, size.width * 4, size.width, size.height).unwrap();
		let surface = Surface::I420(i420);

		let as_rgba = rgba(&surface, &Config::default()).unwrap();
		let as_bgra = bgra(&surface, &Config::default()).unwrap();

		assert_eq!(as_bgra.width(), as_rgba.width());
		assert_eq!(as_bgra.height(), as_rgba.height());
		assert_eq!(as_bgra.stride(), as_rgba.stride());
		for (index, (rgba, bgra)) in as_rgba
			.data()
			.as_chunks::<4>()
			.0
			.iter()
			.zip(as_bgra.data().as_chunks::<4>().0.iter())
			.enumerate()
		{
			assert_eq!(
				[bgra[0], bgra[1], bgra[2], bgra[3]],
				[rgba[2], rgba[1], rgba[0], rgba[3]],
				"pixel {index}: {bgra:?} is not {rgba:?} with red and blue exchanged",
			);
		}
	}

	/// Converting borrows the surface, so the same one converts twice.
	///
	/// The reason this matters is not the second conversion: it is that a
	/// caller holding an `Arc<Frame>` it cannot unwrap, which is every consumer
	/// of a publisher's preview, has no way to reach a consuming exit at all.
	#[test]
	fn conversion_leaves_the_surface_alone() {
		let size = Size::new(32, 32);
		let surface = Surface::I420(I420::new(size.width, size.height, vec![128; I420::len(32, 32)]).unwrap());

		let first = rgba(&surface, &Config::default()).unwrap();
		let second = bgra(&surface, &Config::default()).unwrap();
		assert_eq!(first.data().len(), second.data().len());
		// And the surface is still there to ask again.
		assert_eq!(surface.width(), size.width);
	}
}
