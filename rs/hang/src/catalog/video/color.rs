use serde::{Deserialize, Serialize};

/// The colour a rendition's pixels carry, so a renderer can set up its pipeline without reparsing the bitstream.
///
/// The first four fields are ITU-T H.273 (ISO/IEC 23091-2) code points, the numbering H.264 and
/// H.265 VUI, AV1, VP9, and the ISOBMFF `colr` box all share. Absent means the source left it
/// unspecified, never a signalled default.
///
/// Marked `#[non_exhaustive]`: start from [`Color::default`] and assign the fields you know.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Color {
	/// The chromaticities of the source primaries, as H.273 `ColourPrimaries` (1 is BT.709, 9 is BT.2020).
	pub primaries: Option<u8>,

	/// The transfer function, as H.273 `TransferCharacteristics` (1 is BT.709, 16 is PQ, 18 is HLG).
	pub transfer: Option<u8>,

	/// The matrix between RGB and Y'CbCr, as H.273 `MatrixCoefficients` (0 is RGB, 1 is BT.709, 9 is BT.2020 NCL).
	pub matrix: Option<u8>,

	/// The numeric range of the samples.
	pub range: Option<Range>,

	/// The display the content was mastered on, as SMPTE ST 2086.
	pub mastering: Option<Mastering>,

	/// The content's light levels, as CTA-861.3.
	pub content_light: Option<ContentLight>,
}

/// The numeric range of the samples, as H.273's `VideoFullRangeFlag`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Range {
	/// Studio swing: 8-bit luma spans 16..235 and chroma 16..240.
	Limited,
	/// Full swing: every 8-bit code is used.
	Full,
}

/// The colour volume of a mastering display, as SMPTE ST 2086.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Mastering {
	/// The red primary.
	pub red: Chromaticity,
	/// The green primary.
	pub green: Chromaticity,
	/// The blue primary.
	pub blue: Chromaticity,
	/// The white point.
	pub white: Chromaticity,
	/// The peak luminance in candelas per square metre.
	pub max_luminance: f64,
	/// The minimum luminance in candelas per square metre.
	pub min_luminance: f64,
}

/// A CIE 1931 xy chromaticity coordinate.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct Chromaticity {
	/// The x coordinate.
	pub x: f64,
	/// The y coordinate.
	pub y: f64,
}

/// Content light levels in candelas per square metre, as CTA-861.3. Zero means unknown.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContentLight {
	/// The brightest pixel in the content (MaxCLL).
	pub max_cll: u16,
	/// The brightest frame average in the content (MaxFALL).
	pub max_fall: u16,
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn hdr10_round_trips() {
		let color = Color {
			primaries: Some(9),
			transfer: Some(16),
			matrix: Some(9),
			range: Some(Range::Limited),
			mastering: Some(Mastering {
				red: Chromaticity { x: 0.708, y: 0.292 },
				green: Chromaticity { x: 0.17, y: 0.797 },
				blue: Chromaticity { x: 0.131, y: 0.046 },
				white: Chromaticity { x: 0.3127, y: 0.329 },
				max_luminance: 1000.0,
				min_luminance: 0.005,
			}),
			content_light: Some(ContentLight {
				max_cll: 1000,
				max_fall: 400,
			}),
		};

		let encoded = serde_json::to_value(&color).unwrap();
		assert_eq!(
			encoded,
			serde_json::json!({
				"primaries": 9,
				"transfer": 16,
				"matrix": 9,
				"range": "limited",
				"mastering": {
					"red": { "x": 0.708, "y": 0.292 },
					"green": { "x": 0.17, "y": 0.797 },
					"blue": { "x": 0.131, "y": 0.046 },
					"white": { "x": 0.3127, "y": 0.329 },
					"maxLuminance": 1000.0,
					"minLuminance": 0.005,
				},
				"contentLight": { "maxCll": 1000, "maxFall": 400 },
			})
		);
		assert_eq!(serde_json::from_value::<Color>(encoded).unwrap(), color);
	}

	#[test]
	fn unspecified_fields_are_omitted() {
		let color = Color {
			matrix: Some(1),
			..Default::default()
		};
		assert_eq!(
			serde_json::to_value(&color).unwrap(),
			serde_json::json!({ "matrix": 1 })
		);
	}
}
