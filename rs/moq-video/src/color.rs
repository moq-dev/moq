//! [`Color`]: which YUV color space a frame's samples are in.

use crate::Size;

/// Which YUV color space a frame's samples are in.
///
/// Video carries luma and chroma, not RGB, and the matrix that converts between
/// them differs by generation (BT.601 for standard definition, BT.709 for high
/// definition) as does the numeric range (limited/studio swing pins luma to
/// 16..235, full/full swing uses 0..255). Pairing samples with the wrong matrix
/// is the classic tinted-video bug: it leaves grays untouched and skews
/// saturated colors, so it survives a casual look at the picture.
///
/// [`I420::color`](crate::I420::color) reports it where the crate knows, which
/// is when the crate did the conversion itself. It is `None` for pixels that
/// merely passed through (a decoder's output, a camera's), since the
/// authoritative answer lives in the bitstream's VUI and does not survive into a
/// decoded [`Frame`](crate::Frame). [`Color::infer`] is the fallback then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Color {
	/// BT.601 (standard definition), limited range.
	Bt601Limited,
	/// BT.601 (standard definition), full range.
	Bt601Full,
	/// BT.709 (high definition), limited range.
	Bt709Limited,
	/// BT.709 (high definition), full range.
	Bt709Full,
}

impl Color {
	/// The conventional guess for a frame of this size: BT.601 up to standard
	/// definition (576 lines), BT.709 above it, both limited range.
	///
	/// What a player does when the bitstream carries no VUI color description,
	/// which is most of the time. A guess, so prefer a known [`Color`] whenever
	/// one is available.
	pub fn infer(size: Size) -> Self {
		match size.height <= 576 {
			true => Color::Bt601Limited,
			false => Color::Bt709Limited,
		}
	}

	/// The same matrix as `self` but in the given range, for a caller that knows
	/// the range (a pixel format says so) and not the matrix.
	pub(crate) fn with_range(self, limited: bool) -> Self {
		match (self, limited) {
			(Color::Bt601Limited | Color::Bt601Full, true) => Color::Bt601Limited,
			(Color::Bt601Limited | Color::Bt601Full, false) => Color::Bt601Full,
			(_, true) => Color::Bt709Limited,
			(_, false) => Color::Bt709Full,
		}
	}

	/// Whether luma is 16..235 rather than 0..255.
	pub(crate) fn limited(self) -> bool {
		matches!(self, Color::Bt601Limited | Color::Bt709Limited)
	}

	/// The luma/chroma weights (`kr`, `kb`) this matrix is derived from.
	pub(crate) fn weights(self) -> (f32, f32) {
		match self {
			Color::Bt601Limited | Color::Bt601Full => (0.299, 0.114),
			Color::Bt709Limited | Color::Bt709Full => (0.2126, 0.0722),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn inference_splits_at_standard_definition() {
		assert_eq!(Color::infer(Size::new(720, 480)), Color::Bt601Limited);
		assert_eq!(Color::infer(Size::new(720, 576)), Color::Bt601Limited);
		assert_eq!(Color::infer(Size::new(1280, 720)), Color::Bt709Limited);
	}

	#[test]
	fn with_range_keeps_the_matrix() {
		assert_eq!(Color::Bt709Limited.with_range(false), Color::Bt709Full);
		assert_eq!(Color::Bt709Full.with_range(true), Color::Bt709Limited);
		assert_eq!(Color::Bt601Limited.with_range(false), Color::Bt601Full);
	}
}
