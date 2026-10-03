//! D3D11 processor configuration, testable without a GPU or Windows host.

#[cfg(any(feature = "capture", test))]
use crate::Error;
use crate::{Color, Size};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Plan {
	pub source: Size,
	pub picture: Size,
	pub target: Size,
	pub bgra: bool,
	pub space: u32,
}

impl Plan {
	pub fn resize(source: Size, target: Size, color: Option<Color>) -> Self {
		Self {
			source,
			picture: source,
			target,
			bgra: false,
			space: color.map(space).unwrap_or(0),
		}
	}

	#[cfg(any(feature = "capture", test))]
	pub fn capture(source: Size) -> Result<Self, Error> {
		let target = Size::new(source.width & !1, source.height & !1);
		if target.width == 0 || target.height == 0 || source.width > i32::MAX as u32 || source.height > i32::MAX as u32
		{
			return Err(Error::SourceUnavailable("screen has no capturable area".into()));
		}
		Ok(Self {
			source,
			picture: target,
			target,
			bgra: true,
			space: space(Color::infer(target)),
		})
	}

	pub fn input_space(self) -> u32 {
		if self.bgra { RGB_FULL } else { self.space }
	}
}

// D3D11_VIDEO_PROCESSOR_COLOR_SPACE: RGB_Range is bit 1 (0 = full),
// YCbCr_Matrix bit 2 (1 = 709), Nominal_Range bits 4..6 (1 = limited, 2 = full).
const RGB_FULL: u32 = 2 << 4;

fn space(color: Color) -> u32 {
	let matrix = u32::from(matches!(color, Color::Bt709Limited | Color::Bt709Full));
	let range = if color.limited() { 1 } else { 2 };
	(matrix << 2) | (range << 4)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn odd_capture_crops_instead_of_scaling() {
		let plan = Plan::capture(Size::new(801, 601)).unwrap();
		assert_eq!(plan.source, Size::new(801, 601));
		assert_eq!(plan.picture, Size::new(800, 600));
		assert_eq!(plan.picture, plan.target);
	}

	#[test]
	fn capture_refuses_empty_or_unrepresentable_rectangles() {
		for size in [Size::new(0, 600), Size::new(800, 1), Size::new(u32::MAX, 600)] {
			assert!(Plan::capture(size).is_err());
		}
	}

	#[test]
	fn capture_converts_full_rgb_to_the_declared_matrix_and_range() {
		for (height, matrix) in [(480, 0), (576, 0), (578, 1), (1080, 1)] {
			let plan = Plan::capture(Size::new(1920, height)).unwrap();
			assert_eq!(plan.input_space(), 2 << 4);
			assert_eq!(plan.space, (matrix << 2) | (1 << 4));
		}
	}

	#[test]
	fn resizing_preserves_color_across_the_sd_boundary() {
		for color in [
			Color::Bt601Limited,
			Color::Bt601Full,
			Color::Bt709Limited,
			Color::Bt709Full,
		] {
			let plan = Plan::resize(Size::new(1920, 1080), Size::new(640, 360), Some(color));
			assert_eq!(plan.input_space(), space(color));
			assert_eq!(plan.space, space(color));
		}
		assert_eq!(Plan::resize(Size::new(640, 480), Size::new(320, 240), None).space, 0);
	}

	#[test]
	fn the_cache_distinguishes_conversion_resize_and_color() {
		let size = Size::new(1280, 720);
		let capture = Plan::capture(size).unwrap();
		let resize = Plan::resize(size, size, Some(Color::Bt709Limited));
		let full = Plan::resize(size, size, Some(Color::Bt709Full));
		let unknown = Plan::resize(size, size, None);
		let keys = std::collections::HashSet::from([capture, resize, full, unknown]);
		assert_eq!(keys.len(), 4);
	}
}
