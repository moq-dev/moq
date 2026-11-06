//! [`Color`]: which color space a frame's samples are in.

use crate::Size;

/// Which color space a frame's samples are in: the YUV matrix and range that
/// decode them to RGB, and the primaries and transfer curve that RGB means.
///
/// Video carries luma and chroma, not RGB, and the matrix that converts between
/// them differs by generation (BT.601 for standard definition, BT.709 for high
/// definition, BT.2020 for UHD) as does the numeric range (limited/studio swing
/// pins luma to 16..235, full swing uses 0..255). Pairing samples with the
/// wrong matrix is the classic tinted-video bug: it leaves grays untouched and
/// skews saturated colors, so it survives a casual look at the picture. The
/// primaries and transfer travel separately because sources mix them: a BT.601
/// matrix over BT.709 primaries is common, and HDR changes only the transfer.
///
/// [`Surface::color`](crate::Surface::color) reports it where the crate knows:
/// when the crate did the conversion itself, when a decoder read the stream's
/// VUI, or when the surface carries the answer. It is `None` for pixels that
/// merely passed through with nothing naming their space, a camera's raw YUYV
/// among them. [`Color::infer`] is the fallback then.
///
/// Build one from [`BT601`](Self::BT601) or [`BT709`](Self::BT709) and change
/// the fields that differ:
///
/// ```
/// use moq_video::Color;
/// use moq_video::color::{Primaries, Range};
///
/// let jpeg = Color { range: Range::Full, ..Color::BT601 };
/// let webcam = Color { primaries: Primaries::Bt709, ..Color::BT601 };
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Color {
	/// The matrix between RGB and Y'CbCr.
	pub matrix: Matrix,
	/// The numeric range of the samples.
	pub range: Range,
	/// The chromaticities of the RGB primaries.
	pub primaries: Primaries,
	/// The curve between linear light and the RGB values.
	pub transfer: Transfer,
}

/// The matrix between RGB and Y'CbCr.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Matrix {
	/// BT.601 (SMPTE 170M and BT.470 System B/G define the same matrix).
	Bt601,
	/// BT.709.
	Bt709,
	/// BT.2020 non-constant luminance.
	Bt2020,
}

/// The numeric range of the samples, the same type the catalog's colour carries.
pub use hang::catalog::Range;

/// The chromaticities of the RGB primaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Primaries {
	/// SMPTE 170M, the 525-line (NTSC) BT.601 primaries.
	Smpte170m,
	/// BT.470 System B/G, the 625-line (PAL) BT.601 primaries.
	Bt470bg,
	/// BT.709, which sRGB shares.
	Bt709,
	/// BT.2020, the UHD and HDR gamut.
	Bt2020,
}

/// The curve between linear light and the RGB values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Transfer {
	/// The SDR curve BT.709 defines, which BT.601 and BT.2020 share.
	Bt709,
	/// SMPTE ST 2084 perceptual quantizer, the HDR10 curve.
	Pq,
	/// ARIB STD-B67 hybrid log-gamma.
	Hlg,
}

/// The ISO/IEC 23091-2 code points a VUI carries for a [`Color`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CodePoints {
	pub primaries: u8,
	pub transfer: u8,
	pub matrix: u8,
}

impl Color {
	/// BT.601 as SD video uses it: SMPTE 170M primaries and matrix, the SDR
	/// curve, limited range.
	pub const BT601: Self = Self {
		matrix: Matrix::Bt601,
		range: Range::Limited,
		primaries: Primaries::Smpte170m,
		transfer: Transfer::Bt709,
	};

	/// BT.709 throughout, limited range: HD video.
	pub const BT709: Self = Self {
		matrix: Matrix::Bt709,
		range: Range::Limited,
		primaries: Primaries::Bt709,
		transfer: Transfer::Bt709,
	};

	/// The conventional guess for a frame of this size: [`BT601`](Self::BT601)
	/// up to standard definition (576 lines), [`BT709`](Self::BT709) above it.
	///
	/// What a player does when the bitstream carries no VUI color description,
	/// which is most of the time. A guess, so prefer a known [`Color`] whenever
	/// one is available.
	pub fn infer(size: Size) -> Self {
		match size.height <= 576 {
			true => Self::BT601,
			false => Self::BT709,
		}
	}

	/// The same space in the given range.
	pub(crate) const fn with_range(self, range: Range) -> Self {
		Self { range, ..self }
	}

	/// The VUI code points for this space.
	///
	/// BT.601's matrix goes out as SMPTE 170M (6) whatever the primaries, since
	/// BT.470 System B/G (5) defines the same one. The SDR curve goes out as
	/// BT.709 (1): SMPTE 170M and BT.2020 define the same curve under other
	/// numbers, and 1 is the one every backend can emit. Backends name these
	/// through their own APIs; tests check what they wrote against this.
	#[cfg(test)]
	pub(crate) fn code_points(self) -> CodePoints {
		CodePoints {
			primaries: match self.primaries {
				Primaries::Smpte170m => 6,
				Primaries::Bt470bg => 5,
				Primaries::Bt709 => 1,
				Primaries::Bt2020 => 9,
			},
			transfer: match self.transfer {
				Transfer::Bt709 => 1,
				Transfer::Pq => 16,
				Transfer::Hlg => 18,
			},
			matrix: match self.matrix {
				Matrix::Bt601 => 6,
				Matrix::Bt709 => 1,
				Matrix::Bt2020 => 9,
			},
		}
	}

	/// The space a VUI's code points name, or `None` when it names one this
	/// type cannot hold (an RGB or YCgCo matrix, a film gamut, a gamma curve).
	///
	/// An unspecified field (code point 2, or an absent description) takes the
	/// value [`infer`](Self::infer) guesses for `size`.
	pub(crate) fn from_code_points(codes: Option<CodePoints>, range: Range, size: Size) -> Option<Self> {
		let guess = Self::infer(size).with_range(range);
		let Some(codes) = codes else {
			return Some(guess);
		};
		Some(Self {
			matrix: match codes.matrix {
				2 => guess.matrix,
				1 => Matrix::Bt709,
				5 | 6 => Matrix::Bt601,
				9 => Matrix::Bt2020,
				_ => return None,
			},
			range,
			primaries: match codes.primaries {
				2 => guess.primaries,
				1 => Primaries::Bt709,
				5 => Primaries::Bt470bg,
				// SMPTE 240M shares SMPTE 170M's chromaticities.
				6 | 7 => Primaries::Smpte170m,
				9 => Primaries::Bt2020,
				_ => return None,
			},
			transfer: match codes.transfer {
				2 => guess.transfer,
				1 | 6 | 14 | 15 => Transfer::Bt709,
				16 => Transfer::Pq,
				18 => Transfer::Hlg,
				_ => return None,
			},
		})
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
	#[cfg(any(test, all(target_os = "linux", feature = "nvidia"), all(apple, feature = "render")))]
	pub(crate) fn coefficients(self) -> Coefficients {
		let (kr, kb) = self.matrix.weights();
		let kg = 1.0 - kr - kb;
		let (luma_scale, chroma_scale, luma_offset) = match self.range {
			Range::Limited => (219.0 / 255.0, 224.0 / 255.0, 16.0),
			Range::Full => (1.0, 1.0, 0.0),
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

	/// How the `yuv` crate names this space's matrix and range, for the RGB
	/// conversions.
	pub(crate) fn yuv(self) -> (yuv::YuvRange, yuv::YuvStandardMatrix) {
		let range = match self.range {
			Range::Limited => yuv::YuvRange::Limited,
			Range::Full => yuv::YuvRange::Full,
		};
		let matrix = match self.matrix {
			Matrix::Bt601 => yuv::YuvStandardMatrix::Bt601,
			Matrix::Bt709 => yuv::YuvStandardMatrix::Bt709,
			Matrix::Bt2020 => yuv::YuvStandardMatrix::Bt2020,
		};
		(range, matrix)
	}
}

impl Matrix {
	/// The luma weights (`kr`, `kb`) the matrix is derived from.
	#[cfg(any(test, feature = "render", all(target_os = "linux", feature = "nvidia")))]
	pub(crate) fn weights(self) -> (f32, f32) {
		match self {
			Matrix::Bt601 => (0.299, 0.114),
			Matrix::Bt709 => (0.2126, 0.0722),
			Matrix::Bt2020 => (0.2627, 0.0593),
		}
	}
}

/// The weights of one RGB to Y'CbCr conversion: each output sample is
/// `[r, g, b, offset]` dotted with `(R, G, B, 1)`, all on the 0..255 scale.
#[cfg(any(test, all(target_os = "linux", feature = "nvidia"), all(apple, feature = "render")))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Coefficients {
	pub y: [f32; 4],
	pub u: [f32; 4],
	pub v: [f32; 4],
}

#[cfg(any(test, all(target_os = "linux", feature = "nvidia"), all(apple, feature = "render")))]
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
		assert_eq!(Color::infer(Size::new(720, 480)), Color::BT601);
		assert_eq!(Color::infer(Size::new(720, 576)), Color::BT601);
		assert_eq!(Color::infer(Size::new(1280, 720)), Color::BT709);
	}

	#[test]
	fn code_points_round_trip() {
		let size = Size::new(1920, 1080);
		for color in [
			Color::BT601,
			Color::BT709.with_range(Range::Full),
			Color {
				primaries: Primaries::Bt709,
				..Color::BT601
			},
			Color {
				primaries: Primaries::Bt470bg,
				..Color::BT601
			},
			Color {
				matrix: Matrix::Bt2020,
				primaries: Primaries::Bt2020,
				transfer: Transfer::Pq,
				..Color::BT709
			},
			Color {
				matrix: Matrix::Bt2020,
				primaries: Primaries::Bt2020,
				transfer: Transfer::Hlg,
				..Color::BT709
			},
		] {
			assert_eq!(
				Color::from_code_points(Some(color.code_points()), color.range, size),
				Some(color)
			);
		}
	}

	#[test]
	fn unspecified_code_points_take_the_size_guess() {
		let sd = Size::new(640, 480);
		let hd = Size::new(1920, 1080);
		assert_eq!(Color::from_code_points(None, Range::Limited, hd), Some(Color::BT709));
		assert_eq!(
			Color::from_code_points(None, Range::Full, sd),
			Some(Color::BT601.with_range(Range::Full))
		);
		// A signalled matrix with unspecified primaries and transfer.
		let codes = CodePoints {
			primaries: 2,
			transfer: 2,
			matrix: 1,
		};
		assert_eq!(
			Color::from_code_points(Some(codes), Range::Limited, sd),
			Some(Color {
				matrix: Matrix::Bt709,
				..Color::BT601
			})
		);
		// Same curve under SMPTE 170M's number.
		let codes = CodePoints {
			primaries: 6,
			transfer: 6,
			matrix: 6,
		};
		assert_eq!(
			Color::from_code_points(Some(codes), Range::Limited, hd),
			Some(Color::BT601)
		);
	}

	#[test]
	fn unrepresentable_code_points_are_refused() {
		let hd = Size::new(1920, 1080);
		let rgb = CodePoints {
			primaries: 1,
			transfer: 1,
			matrix: 0,
		};
		assert_eq!(Color::from_code_points(Some(rgb), Range::Full, hd), None);
		let srgb_curve = CodePoints {
			primaries: 1,
			transfer: 13,
			matrix: 1,
		};
		assert_eq!(Color::from_code_points(Some(srgb_curve), Range::Full, hd), None);
	}

	/// The textbook 8-bit values for pure red: BT.709 limited is (63, 102, 240),
	/// full range (54, 99, 255); BT.601 limited is (81, 90, 240).
	#[test]
	fn coefficients_match_the_textbook_values() {
		let red = [255, 0, 0];
		let full = |color: Color| color.with_range(Range::Full);
		assert_eq!(Color::BT709.coefficients().apply(red), [63, 102, 240]);
		assert_eq!(full(Color::BT709).coefficients().apply(red), [54, 99, 255]);
		assert_eq!(Color::BT601.coefficients().apply(red), [81, 90, 240]);
		assert_eq!(full(Color::BT601).coefficients().apply([255; 3]), [255, 128, 128]);
		assert_eq!(Color::BT709.coefficients().apply([0; 3]), [16, 128, 128]);
	}

	/// The coefficients agree with the `yuv` crate's conversion, which every CPU
	/// path uses, so a GPU frame converted with them decodes to the same picture
	/// as the same pixels fed through `Surface::rgba`.
	#[test]
	fn coefficients_agree_with_the_yuv_crate() {
		use yuv::{YuvChromaSubsampling, YuvConversionMode, YuvPlanarImageMut, rgba_to_yuv420};

		let bt2020 = Color {
			matrix: Matrix::Bt2020,
			..Color::BT709
		};
		let colors = [
			Color::BT601,
			Color::BT601.with_range(Range::Full),
			Color::BT709,
			Color::BT709.with_range(Range::Full),
			bt2020,
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
