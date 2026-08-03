//! The YUV -> RGB conversion the shader applies, as the shader's uniform.

use crate::Color;

/// The luma/chroma weights (`kr`, `kb`) a matrix is derived from.
///
/// Lives here rather than on [`Color`] because deriving the matrix is the
/// shader's business; the enum itself only names a space.
fn weights(color: Color) -> (f32, f32) {
	match color {
		Color::Bt601Limited | Color::Bt601Full => (0.299, 0.114),
		Color::Bt709Limited | Color::Bt709Full => (0.2126, 0.0722),
	}
}

/// Whether luma is 16..235 rather than 0..255.
fn limited(color: Color) -> bool {
	matches!(color, Color::Bt601Limited | Color::Bt709Limited)
}

/// The uniform the shader multiplies each sample by: a column-major 3x3 matrix
/// padded to WGSL's 16-byte column stride, then the offsets to subtract from
/// (Y, U, V) before the multiply.
///
/// Deriving the matrix from `kr`/`kb` rather than pasting the usual table of
/// magic constants means a new color space is two numbers, and the limited
/// range scaling stays in one place.
pub(super) fn uniform(color: Color) -> [f32; 16] {
	let (kr, kb) = weights(color);
	let kg = 1.0 - kr - kb;
	// The inverse of the RGB -> YCbCr matrix, which is fully determined by
	// kr/kb: Cb and Cr are the blue/red difference scaled into +-0.5.
	let (mut y, mut cb, mut cr) = (1.0f32, 2.0 * (1.0 - kb), 2.0 * (1.0 - kr));
	let (mut cb_g, mut cr_g) = (-cb * kb / kg, -cr * kr / kg);

	// Limited range: luma spans 219 of 255 codes starting at 16, chroma 224
	// centered on 128. Stretch both back to 0..1 so the matrix output is
	// full-range RGB.
	let y_offset = match limited(color) {
		true => {
			let (luma, chroma) = (255.0 / 219.0, 255.0 / 224.0);
			y *= luma;
			cb *= chroma;
			cr *= chroma;
			cb_g *= chroma;
			cr_g *= chroma;
			16.0 / 255.0
		}
		false => 0.0,
	};

	#[rustfmt::skip]
	let uniform = [
		// Column 0: the Y coefficient of R, G, B. Padded to vec4.
		y, y, y, 0.0,
		// Column 1: U (Cb).
		0.0, cb_g, cb, 0.0,
		// Column 2: V (Cr).
		cr, cr_g, 0.0, 0.0,
		// Offsets subtracted from (Y, U, V) before the multiply.
		y_offset, 0.5, 0.5, 0.0,
	];
	uniform
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Convert one YUV triple through the same math the shader runs, so the
	/// expectations below read as pixels rather than matrix entries.
	fn convert(color: Color, yuv: [f32; 3]) -> [f32; 3] {
		let u = uniform(color);
		let offset = [u[12], u[13], u[14]];
		let (y, cb, cr) = (yuv[0] - offset[0], yuv[1] - offset[1], yuv[2] - offset[2]);
		[
			u[0] * y + u[4] * cb + u[8] * cr,
			u[1] * y + u[5] * cb + u[9] * cr,
			u[2] * y + u[6] * cb + u[10] * cr,
		]
	}

	fn assert_rgb(actual: [f32; 3], expected: [f32; 3]) {
		for (a, e) in actual.iter().zip(expected.iter()) {
			assert!((a - e).abs() < 0.01, "got {actual:?}, expected {expected:?}");
		}
	}

	#[test]
	fn limited_range_black_and_white_reach_the_full_rgb_range() {
		// The whole point of the range scaling: code 16 is black, 235 is white.
		for color in [Color::Bt601Limited, Color::Bt709Limited] {
			assert_rgb(convert(color, [16.0 / 255.0, 0.5, 0.5]), [0.0, 0.0, 0.0]);
			assert_rgb(convert(color, [235.0 / 255.0, 0.5, 0.5]), [1.0, 1.0, 1.0]);
		}
	}

	#[test]
	fn full_range_maps_zero_and_one_straight_through() {
		for color in [Color::Bt601Full, Color::Bt709Full] {
			assert_rgb(convert(color, [0.0, 0.5, 0.5]), [0.0, 0.0, 0.0]);
			assert_rgb(convert(color, [1.0, 0.5, 0.5]), [1.0, 1.0, 1.0]);
		}
	}

	#[test]
	fn primaries_round_trip_through_the_matrix() {
		// Encode each primary to YUV with the forward matrix, then check the
		// shader's inverse brings it back. Catches a transposed or mis-scaled
		// column, which a grayscale-only test would miss.
		for color in [Color::Bt601Full, Color::Bt709Full, Color::Bt709Limited] {
			let (kr, kb) = weights(color);
			let kg = 1.0 - kr - kb;
			let (scale, offset) = match limited(color) {
				true => (219.0 / 255.0, 16.0 / 255.0),
				false => (1.0, 0.0),
			};
			let chroma_scale = match limited(color) {
				true => 224.0 / 255.0,
				false => 1.0,
			};

			for rgb in [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0], [0.3, 0.6, 0.9]] {
				let luma = kr * rgb[0] + kg * rgb[1] + kb * rgb[2];
				let yuv = [
					luma * scale + offset,
					(rgb[2] - luma) / (2.0 * (1.0 - kb)) * chroma_scale + 0.5,
					(rgb[0] - luma) / (2.0 * (1.0 - kr)) * chroma_scale + 0.5,
				];
				assert_rgb(convert(color, yuv), rgb);
			}
		}
	}
}
