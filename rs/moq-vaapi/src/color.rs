//! The YUV color space an encoded stream is labelled with.
//!
//! One value serves two jobs that have to agree: the encoder writes it into the
//! SPS video usability information, and the post-processor uses it as the
//! target of every conversion it runs for the encoder. Pixels the encoder
//! converts therefore match their label. Pixels it takes as they are (a CPU
//! upload, a YUV buffer with no color of its own) are labelled, not converted.

use crate::bindings;

/// The matrix that maps RGB to Y'CbCr.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
	/// ITU-R BT.601, the standard-definition matrix.
	Bt601,
	/// ITU-R BT.709, the high-definition matrix.
	Bt709,
}

/// A YUV color space: the matrix and whether samples use the full 8-bit range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
	/// The RGB to Y'CbCr matrix.
	pub matrix: Matrix,
	/// Whether luma spans 0..=255 rather than 16..=235.
	pub full_range: bool,
}

impl Color {
	/// BT.601 in limited range.
	pub const BT601: Self = Self {
		matrix: Matrix::Bt601,
		full_range: false,
	};
	/// BT.709 in limited range.
	pub const BT709: Self = Self {
		matrix: Matrix::Bt709,
		full_range: false,
	};

	/// Returns the conventional space for a stream of `height` lines.
	///
	/// Standard definition (576 lines or fewer) is BT.601, anything taller
	/// BT.709, both in limited range. That is the guess most players make for
	/// an unlabelled stream, though not all of them (ffmpeg's scaler takes
	/// BT.601 at every size), which is why the stream is labelled.
	pub fn infer(height: u32) -> Self {
		match height <= 576 {
			true => Self::BT601,
			false => Self::BT709,
		}
	}

	/// Returns the H.264 VUI `(colour_primaries, transfer_characteristics, matrix_coefficients)`.
	///
	/// BT.601 goes out as SMPTE 170M primaries and matrix (code point 6) with the
	/// BT.709 transfer curve (1), since the two standards differ in primaries
	/// and matrix but not in gamma.
	pub(crate) fn vui(self) -> (u8, u8, u8) {
		match self.matrix {
			Matrix::Bt601 => (6, 1, 6),
			Matrix::Bt709 => (1, 1, 1),
		}
	}

	/// Returns the `VAProcColorStandardType` of the matrix.
	pub(crate) fn va_standard(self) -> u8 {
		(match self.matrix {
			Matrix::Bt601 => bindings::_VAProcColorStandardType_VAProcColorStandardBT601,
			Matrix::Bt709 => bindings::_VAProcColorStandardType_VAProcColorStandardBT709,
		}) as u8
	}

	/// Returns the `VA_SOURCE_RANGE_*` of the range.
	pub(crate) fn va_range(self) -> u8 {
		(match self.full_range {
			true => bindings::VA_SOURCE_RANGE_FULL,
			false => bindings::VA_SOURCE_RANGE_REDUCED,
		}) as u8
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn inference_splits_at_standard_definition() {
		assert_eq!(Color::infer(480), Color::BT601);
		assert_eq!(Color::infer(576), Color::BT601);
		assert_eq!(Color::infer(720), Color::BT709);
	}
}
