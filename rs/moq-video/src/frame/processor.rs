//! D3D11 processor configuration, testable without a GPU or Windows host.

use crate::color::{Matrix, Range};
use crate::{Color, Error, Size};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Plan {
	pub source: Size,
	pub picture: Size,
	pub target: Size,
	pub bgra: bool,
	pub space: u32,
}

impl Plan {
	pub fn resize(source: Size, target: Size, color: Option<Color>) -> Result<Self, Error> {
		Ok(Self {
			source,
			picture: source,
			target,
			bgra: false,
			space: color.map(space).transpose()?.unwrap_or(0),
		})
	}

	/// The output size of the BGRA pass of a capture of `picture` scaled to
	/// `scaled`. A second pass scales NV12 from there to `scaled`.
	///
	/// A video processor can filter BGRA input poorly: on an NVIDIA RTX 500
	/// (driver 32.0.15.9713), one pass from 3072x1920 to 640x360 aliased text
	/// badly, while its NV12 scaler gave a smooth picture at the same ratio.
	/// Up to a halving the BGRA pass is clean. So a capture to less than half
	/// the picture converts at half the size, and the NV12 scaler does the
	/// rest. That writes a quarter of the pixels of a full-size conversion.
	#[cfg(any(feature = "capture", all(target_os = "windows", feature = "render"), test))]
	pub fn capture_stage(picture: Size, scaled: Size) -> Size {
		if scaled.width * 2 <= picture.width && scaled.height * 2 <= picture.height {
			Size::new((picture.width / 2) & !1, (picture.height / 2) & !1)
		} else {
			scaled
		}
	}

	/// Converts BGRA to NV12 and crops odd edges. The output has the even
	/// picture size, or `scaled` if set.
	#[cfg(any(feature = "capture", all(target_os = "windows", feature = "render"), test))]
	pub fn capture(source: Size, scaled: Option<Size>) -> Result<Self, Error> {
		let picture = Size::new(source.width & !1, source.height & !1);
		if picture.width == 0
			|| picture.height == 0
			|| source.width > i32::MAX as u32
			|| source.height > i32::MAX as u32
		{
			return Err(Error::SourceUnavailable("screen has no capturable area".into()));
		}
		let target = match scaled {
			None => picture,
			Some(size) => {
				let even = Size::new(size.width & !1, size.height & !1);
				if even.width == 0 || even.height == 0 || even.width > i32::MAX as u32 || even.height > i32::MAX as u32
				{
					return Err(Error::UnsupportedConfig(format!("cannot capture at {size:?}")));
				}
				even
			}
		};
		Ok(Self {
			source,
			picture,
			target,
			bgra: true,
			space: space(Color::infer(target))?,
		})
	}

	/// The same plan, converting into `color` instead.
	#[cfg(any(feature = "capture", all(target_os = "windows", feature = "render"), test))]
	pub fn with_color(self, color: Color) -> Result<Self, Error> {
		Ok(Self {
			space: space(color)?,
			..self
		})
	}

	pub fn input_space(self) -> u32 {
		if self.bgra { RGB_FULL } else { self.space }
	}
}

// D3D11_VIDEO_PROCESSOR_COLOR_SPACE: RGB_Range is bit 1 (0 = full),
// YCbCr_Matrix bit 2 (1 = 709), Nominal_Range bits 4..6 (1 = limited, 2 = full).
const RGB_FULL: u32 = 2 << 4;

/// The D3D11 color space of `color`, which names only the BT.601 and BT.709
/// matrices.
fn space(color: Color) -> Result<u32, Error> {
	let matrix = match color.matrix {
		Matrix::Bt601 => 0,
		Matrix::Bt709 => 1,
		Matrix::Bt2020 => return Err(Error::Unsupported(format!("D3D11 cannot process {color:?}"))),
	};
	let range = match color.range {
		Range::Limited => 1,
		Range::Full => 2,
	};
	Ok((matrix << 2) | (range << 4))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn odd_capture_crops_instead_of_scaling() {
		let plan = Plan::capture(Size::new(801, 601), None).unwrap();
		assert_eq!(plan.source, Size::new(801, 601));
		assert_eq!(plan.picture, Size::new(800, 600));
		assert_eq!(plan.picture, plan.target);
	}

	#[test]
	fn capture_can_scale_in_the_same_pass() {
		let plan = Plan::capture(Size::new(3071, 1920), Some(Size::new(641, 360))).unwrap();
		assert_eq!(plan.picture, Size::new(3070, 1920));
		assert_eq!(plan.target, Size::new(640, 360));
		// The color follows the output, as for a capture at the native size.
		assert_eq!(plan.space, space(Color::infer(plan.target)).unwrap());
		assert!(Plan::capture(Size::new(1920, 1080), Some(Size::new(1, 360))).is_err());
	}

	#[test]
	fn a_large_downscale_converts_at_half_the_size_first() {
		let target = Size::new(640, 360);
		assert_eq!(Plan::capture_stage(Size::new(3072, 1920), target), Size::new(1536, 960));
		assert_eq!(Plan::capture_stage(Size::new(1282, 722), target), Size::new(640, 360));
		// Less than a halving, or a halving in one direction only, is one pass.
		assert_eq!(Plan::capture_stage(Size::new(1278, 1080), target), target);
		assert_eq!(Plan::capture_stage(Size::new(1920, 700), target), target);
	}

	#[test]
	fn the_stage_converts_into_the_color_of_the_output() {
		let stage = Plan::capture(Size::new(3072, 1920), Some(Size::new(1536, 960))).unwrap();
		assert_eq!(stage.space, space(Color::BT709).unwrap());
		let plan = stage.with_color(Color::infer(Size::new(640, 360))).unwrap();
		assert_eq!(plan.space, space(Color::BT601).unwrap());
		assert_eq!(plan.input_space(), RGB_FULL);
		assert_eq!(plan.target, stage.target);
	}

	#[test]
	fn capture_refuses_empty_or_unrepresentable_rectangles() {
		for size in [Size::new(0, 600), Size::new(800, 1), Size::new(u32::MAX, 600)] {
			assert!(Plan::capture(size, None).is_err());
		}
	}

	#[test]
	fn capture_converts_full_rgb_to_the_declared_matrix_and_range() {
		for (height, matrix) in [(480, 0), (576, 0), (578, 1), (1080, 1)] {
			let plan = Plan::capture(Size::new(1920, height), None).unwrap();
			assert_eq!(plan.input_space(), 2 << 4);
			assert_eq!(plan.space, (matrix << 2) | (1 << 4));
		}
	}

	#[test]
	fn resizing_preserves_color_across_the_sd_boundary() {
		for color in [Color::BT601, Color::BT709] {
			for color in [color, color.with_range(Range::Full)] {
				let plan = Plan::resize(Size::new(1920, 1080), Size::new(640, 360), Some(color)).unwrap();
				assert_eq!(plan.input_space(), space(color).unwrap());
				assert_eq!(plan.space, space(color).unwrap());
			}
		}
		assert_eq!(
			Plan::resize(Size::new(640, 480), Size::new(320, 240), None)
				.unwrap()
				.space,
			0
		);
		let hdr = Color {
			matrix: Matrix::Bt2020,
			..Color::BT709
		};
		assert!(Plan::resize(Size::new(640, 480), Size::new(320, 240), Some(hdr)).is_err());
	}

	#[test]
	fn the_cache_distinguishes_conversion_resize_and_color() {
		let size = Size::new(1280, 720);
		let capture = Plan::capture(size, None).unwrap();
		let resize = Plan::resize(size, size, Some(Color::BT709)).unwrap();
		let full = Plan::resize(size, size, Some(Color::BT709.with_range(Range::Full))).unwrap();
		let unknown = Plan::resize(size, size, None).unwrap();
		let keys = std::collections::HashSet::from([capture, resize, full, unknown]);
		assert_eq!(keys.len(), 4);
	}
}
