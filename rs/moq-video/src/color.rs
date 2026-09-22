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

	/// The 8-bit RGB to Y'CbCr coefficients of this space, for a conversion the
	/// crate runs itself (the GPU kernels).
	///
	/// Compiled for every test build so the cross-check against the `yuv` crate
	/// runs without a GPU.
	///
	/// Display-referred RGB in: the samples are taken as already gamma-encoded,
	/// which is what an 8-bit render target holds, so no transfer function is
	/// applied on the way through. The offsets put chroma at 128 and, for limited
	/// range, luma at 16; the scales fit the range (219/224 of 255 for limited,
	/// all of it for full).
	#[cfg(any(test, all(target_os = "linux", feature = "nvidia")))]
	pub(crate) fn coefficients(self) -> Coefficients {
		let (kr, kb) = match self {
			Color::Bt601Limited | Color::Bt601Full => (0.299, 0.114),
			Color::Bt709Limited | Color::Bt709Full => (0.2126, 0.0722),
		};
		let kg = 1.0 - kr - kb;
		let (luma_scale, chroma_scale, luma_offset) = match self.limited() {
			true => (219.0 / 255.0, 224.0 / 255.0, 16.0),
			false => (1.0, 1.0, 0.0),
		};
		// Cb = (B - Y') / (2 (1 - Kb)) and Cr = (R - Y') / (2 (1 - Kr)), with Y'
		// substituted so each channel is one weighted sum.
		let cb = chroma_scale / (2.0 * (1.0 - kb));
		let cr = chroma_scale / (2.0 * (1.0 - kr));
		Coefficients {
			y: [luma_scale * kr, luma_scale * kg, luma_scale * kb, luma_offset],
			u: [-cb * kr, -cb * kg, cb * (1.0 - kb), 128.0],
			v: [cr * (1.0 - kr), -cr * kg, -cr * kb, 128.0],
		}
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

/// The weights of one RGB to Y'CbCr conversion: each output sample is
/// `[r, g, b, offset]` dotted with `(R, G, B, 1)`, all on the 0..255 scale.
#[cfg(any(test, all(target_os = "linux", feature = "nvidia")))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Coefficients {
	pub y: [f32; 4],
	pub u: [f32; 4],
	pub v: [f32; 4],
}

#[cfg(any(test, all(target_os = "linux", feature = "nvidia")))]
impl Coefficients {
	/// One pixel through the matrix, rounded and clamped the way the kernels do
	/// it. The CPU reference for a GPU conversion, and what the tests compare
	/// against the `yuv` crate.
	#[cfg(test)]
	pub(crate) fn apply(&self, rgb: [u8; 3]) -> [u8; 3] {
		let dot = |w: [f32; 4]| {
			let v = w[0] * rgb[0] as f32 + w[1] * rgb[1] as f32 + w[2] * rgb[2] as f32 + w[3];
			v.round().clamp(0.0, 255.0) as u8
		};
		[dot(self.y), dot(self.u), dot(self.v)]
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

	/// The textbook 8-bit values for pure red: BT.709 limited is (63, 102, 240),
	/// full range (54, 99, 255); BT.601 limited is (81, 90, 240).
	#[test]
	fn coefficients_match_the_textbook_values() {
		let red = [255, 0, 0];
		assert_eq!(Color::Bt709Limited.coefficients().apply(red), [63, 102, 240]);
		assert_eq!(Color::Bt709Full.coefficients().apply(red), [54, 99, 255]);
		assert_eq!(Color::Bt601Limited.coefficients().apply(red), [81, 90, 240]);
		assert_eq!(Color::Bt601Full.coefficients().apply([255; 3]), [255, 128, 128]);
		assert_eq!(Color::Bt709Limited.coefficients().apply([0; 3]), [16, 128, 128]);
	}

	/// The coefficients agree with the `yuv` crate's conversion, which every CPU
	/// path uses, so a GPU frame converted with them decodes to the same picture
	/// as the same pixels fed through `Surface::rgba`.
	#[test]
	fn coefficients_agree_with_the_yuv_crate() {
		use yuv::{YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, rgba_to_yuv420};

		let colors = [
			Color::Bt601Limited,
			Color::Bt601Full,
			Color::Bt709Limited,
			Color::Bt709Full,
		];
		let pixels: [[u8; 3]; 6] = [
			[255, 0, 0],
			[0, 255, 0],
			[0, 0, 255],
			[255, 255, 255],
			[17, 200, 90],
			[128, 128, 128],
		];
		for color in colors {
			let (range, matrix) = color.yuv();
			let coefficients = color.coefficients();
			for rgb in pixels {
				// A solid 2x2 block, so the crate's chroma subsampling changes nothing.
				let rgba: Vec<u8> = std::iter::repeat_n([rgb[0], rgb[1], rgb[2], 255], 4)
					.flatten()
					.collect();
				let mut planar = YuvPlanarImageMut::alloc(2, 2, YuvChromaSubsampling::Yuv420);
				rgba_to_yuv420(&mut planar, &rgba, 8, range, matrix, YuvConversionMode::Balanced).unwrap();
				let expected = [
					planar.y_plane.borrow()[0],
					planar.u_plane.borrow()[0],
					planar.v_plane.borrow()[0],
				];
				let actual = coefficients.apply(rgb);
				for (channel, (a, e)) in actual.iter().zip(expected).enumerate() {
					assert!(
						a.abs_diff(e) <= 1,
						"{color:?} {rgb:?} channel {channel}: coefficients {actual:?}, yuv crate {expected:?}"
					);
				}
			}
		}
	}
}
