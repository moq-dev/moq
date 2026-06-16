//! VP9.
//!
//! Parses the VP9 uncompressed frame header (VP9 bitstream spec §6.2, §7.2) to
//! detect key frames and read the dimensions, profile, bit depth, and color
//! config, and provides an [`Import`] that publishes a raw VP9 bitstream (one
//! frame per buffer) to a moq broadcast.
//!
//! VP9 stores its codec config in a `vpcC` box, so `vpcc` builds that record
//! for the fMP4 exporter. It's the exact inverse of the `Vp09` import mapping.

mod import;

pub use import::*;

use hang::catalog::VP9;

/// VP9 parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("invalid VP9 frame marker")]
	InvalidFrameMarker,

	#[error("invalid VP9 sync code")]
	InvalidSyncCode,

	#[error("VP9 header truncated")]
	Truncated,

	#[error("empty VP9 frame")]
	EmptyFrame,
}

/// A Result type alias for VP9 parsing.
pub type Result<T> = std::result::Result<T, Error>;

/// VP9 key-frame sync code (VP9 spec §6.2, `frame_sync_code`).
const SYNC_CODE: u32 = 0x49_8342;

/// Fields parsed from a VP9 uncompressed frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
	/// True for a key frame (`frame_type == 0`).
	pub keyframe: bool,
	/// Encoded `(width, height)` and color config, present only on key frames.
	pub key: Option<KeyFrame>,
}

/// The parts of a VP9 key-frame header we surface to the catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct KeyFrame {
	pub width: u16,
	pub height: u16,
	pub profile: u8,
	pub bit_depth: u8,
	/// `vpcC` chroma subsampling enum (0 = 4:2:0 vertical, 1 = 4:2:0 colocated,
	/// 2 = 4:2:2, 3 = 4:4:4).
	pub chroma_subsampling: u8,
	/// CICP matrix coefficients derived from the VP9 `color_space`.
	pub matrix_coefficients: u8,
	pub full_range: bool,
}

impl FrameHeader {
	/// Parse the VP9 uncompressed header.
	///
	/// Reads only as far as the frame size (the last field we care about);
	/// everything after it is left untouched.
	pub fn parse(data: &[u8]) -> Result<Self> {
		let mut r = BitReader::new(data);

		if r.read(2)? != 0b10 {
			return Err(Error::InvalidFrameMarker);
		}

		let profile_low = r.read(1)?;
		let profile_high = r.read(1)?;
		let profile = ((profile_high << 1) | profile_low) as u8;
		if profile == 3 {
			r.skip(1)?; // reserved_zero
		}

		// show_existing_frame: displays a previously decoded frame, no header follows.
		if r.read(1)? == 1 {
			r.skip(3)?; // frame_to_show_map_idx
			return Ok(Self {
				keyframe: false,
				key: None,
			});
		}

		let keyframe = r.read(1)? == 0; // frame_type: 0 = KEY_FRAME
		r.skip(2)?; // show_frame, error_resilient_mode

		if !keyframe {
			return Ok(Self {
				keyframe: false,
				key: None,
			});
		}

		if r.read(24)? != SYNC_CODE {
			return Err(Error::InvalidSyncCode);
		}

		// color_config (VP9 spec §6.2.2).
		let bit_depth = if profile >= 2 {
			if r.read(1)? == 1 { 12 } else { 10 }
		} else {
			8
		};
		let color_space = r.read(3)? as u8;

		const CS_RGB: u8 = 7;
		let (subsampling_x, subsampling_y, full_range);
		if color_space != CS_RGB {
			full_range = r.read(1)? == 1;
			if profile == 1 || profile == 3 {
				subsampling_x = r.read(1)? == 1;
				subsampling_y = r.read(1)? == 1;
				r.skip(1)?; // reserved_zero
			} else {
				// Profiles 0 and 2 are 4:2:0 only.
				(subsampling_x, subsampling_y) = (true, true);
			}
		} else {
			// CS_RGB is full-range 4:4:4, allowed only with profile 1 or 3.
			full_range = true;
			(subsampling_x, subsampling_y) = (false, false);
			if profile == 1 || profile == 3 {
				r.skip(1)?; // reserved_zero
			}
		}

		// frame_size (VP9 spec §6.2.3): each field is `value - 1`.
		let width = (r.read(16)? + 1) as u16;
		let height = (r.read(16)? + 1) as u16;

		Ok(Self {
			keyframe: true,
			key: Some(KeyFrame {
				width,
				height,
				profile,
				bit_depth,
				chroma_subsampling: chroma_subsampling(subsampling_x, subsampling_y),
				matrix_coefficients: matrix_coefficients(color_space),
				full_range,
			}),
		})
	}
}

impl KeyFrame {
	/// Build the catalog VP9 config from a parsed key frame.
	///
	/// Color primaries and transfer characteristics aren't recoverable from the
	/// VP9 `color_space` alone, so they're left unspecified (2), matching what
	/// remuxers like ffmpeg do. `level` is the minimum that fits the picture
	/// size (see [`level_for`]).
	pub fn to_catalog(self) -> VP9 {
		VP9 {
			profile: self.profile,
			level: level_for(self.width, self.height),
			bit_depth: self.bit_depth,
			chroma_subsampling: self.chroma_subsampling,
			color_primaries: 2,
			transfer_characteristics: 2,
			matrix_coefficients: self.matrix_coefficients,
			full_range: self.full_range,
		}
	}
}

/// `vpcC` chroma subsampling enum from the VP9 `subsampling_x`/`subsampling_y`
/// flags. VP9 doesn't signal chroma siting, so 4:2:0 maps to "colocated" (1),
/// the value encoders conventionally write.
fn chroma_subsampling(x: bool, y: bool) -> u8 {
	match (x, y) {
		(true, true) => 1,   // 4:2:0
		(true, false) => 2,  // 4:2:2
		(false, false) => 3, // 4:4:4
		(false, true) => 0,  // 4:4:0 (rare)
	}
}

/// CICP matrix coefficients for a VP9 `color_space` (VP9 spec Table in §7.2.2),
/// matching ffmpeg's `vp9_colorspaces`. Unknown/reserved map to unspecified (2).
fn matrix_coefficients(color_space: u8) -> u8 {
	match color_space {
		1 => 5, // CS_BT_601  -> BT.470BG
		2 => 1, // CS_BT_709  -> BT.709
		3 => 6, // CS_SMPTE_170 -> SMPTE 170M
		4 => 7, // CS_SMPTE_240 -> SMPTE 240M
		5 => 9, // CS_BT_2020 -> BT.2020 non-constant luminance
		7 => 0, // CS_RGB     -> identity
		_ => 2, // CS_UNKNOWN / CS_RESERVED -> unspecified
	}
}

/// The lowest VP9 level whose `MaxLumaPictureSize` fits `width * height` (VP9
/// spec Annex A). Framerate and bitrate aren't known from the header, so this
/// is a picture-size lower bound rather than the exact encoded level.
fn level_for(width: u16, height: u16) -> u8 {
	let area = width as u64 * height as u64;
	// (level, MaxLumaPictureSize), ascending.
	const LEVELS: &[(u8, u64)] = &[
		(10, 36_864),
		(11, 73_728),
		(20, 122_880),
		(21, 245_760),
		(30, 552_960),
		(31, 983_040),
		(40, 2_228_224),
		(50, 8_912_896),
		(60, 35_651_584),
		(62, 70_254_592),
	];
	LEVELS
		.iter()
		.find(|(_, max)| area <= *max)
		.map(|(level, _)| *level)
		.unwrap_or(62)
}

/// Build the `vpcC` configuration record for VP9.
///
/// The exact inverse of the `Vp09` -> catalog mapping in the fMP4 importer.
pub(crate) fn vpcc(vp9: &VP9) -> mp4_atom::VpcC {
	mp4_atom::VpcC {
		profile: vp9.profile,
		level: vp9.level,
		bit_depth: vp9.bit_depth,
		chroma_subsampling: vp9.chroma_subsampling,
		video_full_range_flag: vp9.full_range,
		color_primaries: vp9.color_primaries,
		transfer_characteristics: vp9.transfer_characteristics,
		matrix_coefficients: vp9.matrix_coefficients,
		codec_initialization_data: Vec::new(),
	}
}

/// Minimal MSB-first bit reader over a byte slice.
struct BitReader<'a> {
	data: &'a [u8],
	bit: usize,
}

impl<'a> BitReader<'a> {
	fn new(data: &'a [u8]) -> Self {
		Self { data, bit: 0 }
	}

	fn read(&mut self, n: u32) -> Result<u32> {
		let mut value = 0;
		for _ in 0..n {
			let byte = self.bit / 8;
			if byte >= self.data.len() {
				return Err(Error::Truncated);
			}
			let shift = 7 - (self.bit % 8);
			value = (value << 1) | u32::from((self.data[byte] >> shift) & 1);
			self.bit += 1;
		}
		Ok(value)
	}

	fn skip(&mut self, n: u32) -> Result<()> {
		self.read(n).map(|_| ())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	// A real-shape VP9 key frame: profile 0, show_frame, 8-bit, CS_BT_601,
	// studio range, 4:2:0, 320x240. Bytes after the frame size are irrelevant.
	const KEYFRAME_320X240: &[u8] = &[0x82, 0x49, 0x83, 0x42, 0x20, 0x13, 0xf0, 0x0e, 0xf0, 0x00];

	#[test]
	fn parses_keyframe() {
		let header = FrameHeader::parse(KEYFRAME_320X240).expect("parse key frame");
		assert!(header.keyframe);
		let key = header.key.expect("key frame fields");
		assert_eq!((key.width, key.height), (320, 240));
		assert_eq!(key.profile, 0);
		assert_eq!(key.bit_depth, 8);
		assert_eq!(key.chroma_subsampling, 1); // 4:2:0
		assert_eq!(key.matrix_coefficients, 5); // CS_BT_601 -> BT.470BG
		assert!(!key.full_range);
	}

	#[test]
	fn keyframe_to_catalog() {
		let key = FrameHeader::parse(KEYFRAME_320X240).unwrap().key.unwrap();
		let vp9 = key.to_catalog();
		assert_eq!(vp9.profile, 0);
		assert_eq!(vp9.bit_depth, 8);
		assert_eq!(vp9.level, 20); // 320x240 = 76800, above level 1.1's 73728
		assert_eq!(vp9.color_primaries, 2); // unspecified
	}

	#[test]
	fn parses_interframe() {
		// Non-key frame: marker(10) profile(00) show_existing(0) frame_type(1) ...
		// 0b10_00_0_1_00 = 0x84.
		let header = FrameHeader::parse(&[0x84, 0x00, 0x00]).expect("parse interframe");
		assert!(!header.keyframe);
		assert!(header.key.is_none());
	}

	#[test]
	fn parses_show_existing() {
		// marker(10) profile(00) show_existing(1) idx(000) = 0b10_00_1_000 = 0x88.
		let header = FrameHeader::parse(&[0x88]).expect("parse show_existing");
		assert!(!header.keyframe);
		assert!(header.key.is_none());
	}

	#[test]
	fn rejects_bad_sync() {
		let mut frame = KEYFRAME_320X240.to_vec();
		frame[1] = 0x00; // corrupt the sync code
		assert!(FrameHeader::parse(&frame).is_err());
	}

	#[test]
	fn vpcc_round_trips_catalog() {
		let vp9 = VP9 {
			profile: 2,
			level: 31,
			bit_depth: 10,
			chroma_subsampling: 1,
			color_primaries: 9,
			transfer_characteristics: 16,
			matrix_coefficients: 9,
			full_range: true,
		};
		let vpcc = vpcc(&vp9);
		assert_eq!(vpcc.profile, 2);
		assert_eq!(vpcc.bit_depth, 10);
		assert_eq!(vpcc.matrix_coefficients, 9);
		assert!(vpcc.video_full_range_flag);
	}
}
