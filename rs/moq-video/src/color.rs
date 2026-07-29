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
/// [`Surface::color`](crate::Surface::color) reports it where the crate knows:
/// when the crate did the conversion itself, or when the surface carries the
/// answer (a macOS pixel buffer names its matrix, which VideoToolbox copies out
/// of the stream's VUI). It is `None` for pixels that merely passed through with
/// nothing naming their space, a camera's raw YUYV among them.
/// [`Color::infer`] is the fallback then.
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
	/// the range and not the matrix.
	///
	/// Only a surface whose pixel format spells out its range reaches this, which
	/// is why it is macOS-only: CoreVideo's video-range and full-range NV12 name
	/// theirs.
	#[cfg(target_os = "macos")]
	pub(crate) fn with_range(self, limited: bool) -> Self {
		match (self, limited) {
			(Color::Bt601Limited | Color::Bt601Full, true) => Color::Bt601Limited,
			(Color::Bt601Limited | Color::Bt601Full, false) => Color::Bt601Full,
			(_, true) => Color::Bt709Limited,
			(_, false) => Color::Bt709Full,
		}
	}

	/// Whether luma is 16..235 rather than 0..255.
	///
	/// The encoders need it for the VUI's `video_full_range_flag`, so unlike the
	/// render module's `weights` it is not render-only.
	pub(crate) fn limited(self) -> bool {
		matches!(self, Color::Bt601Limited | Color::Bt709Limited)
	}

	/// How the `yuv` crate names this color space, for the RGB conversions.
	pub(crate) fn yuv(self) -> (yuv::YuvRange, yuv::YuvStandardMatrix) {
		let range = match self.limited() {
			true => yuv::YuvRange::Limited,
			false => yuv::YuvRange::Full,
		};
		let matrix = match self {
			Color::Bt601Limited | Color::Bt601Full => yuv::YuvStandardMatrix::Bt601,
			Color::Bt709Limited | Color::Bt709Full => yuv::YuvStandardMatrix::Bt709,
		};
		(range, matrix)
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

	#[cfg(target_os = "macos")]
	#[test]
	fn with_range_keeps_the_matrix() {
		assert_eq!(Color::Bt709Limited.with_range(false), Color::Bt709Full);
		assert_eq!(Color::Bt709Full.with_range(true), Color::Bt709Limited);
		assert_eq!(Color::Bt601Limited.with_range(false), Color::Bt601Full);
	}
}
