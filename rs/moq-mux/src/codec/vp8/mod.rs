//! VP8.
//!
//! Parses the VP8 uncompressed frame header (RFC 6386 §9.1, §19.1) to detect
//! key frames and read the encoded dimensions, and provides an [`Import`] that
//! publishes a raw VP8 bitstream (one frame per buffer) to a moq broadcast.
//!
//! VP8 carries no out-of-band configuration record, so `vpcc` synthesizes the
//! informational `vpcC` box the fMP4 exporter needs.

mod import;

pub use import::*;

/// VP8 parsing errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("VP8 frame too short for tag")]
	FrameTooShort,

	#[error("VP8 key frame too short for header")]
	KeyframeHeaderTooShort,

	#[error("VP8 key frame start code mismatch")]
	StartCodeMismatch,

	#[error("empty VP8 frame")]
	EmptyFrame,
}

/// A Result type alias for VP8 parsing.
pub type Result<T> = std::result::Result<T, Error>;

/// Fields parsed from a VP8 frame tag (RFC 6386 §9.1) plus, for key frames, the
/// key-frame header (§19.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameHeader {
	/// True for a key frame (frame_type bit clear), false for an interframe.
	pub keyframe: bool,
	/// Encoded `(width, height)`, present only on key frames.
	pub dimensions: Option<(u16, u16)>,
}

impl FrameHeader {
	/// Parse the leading bytes of a VP8 frame.
	///
	/// Reads the 3-byte frame tag, and on a key frame the 7-byte header that
	/// follows (start code + dimensions). Interframes carry neither a start code
	/// nor dimensions.
	pub fn parse(data: &[u8]) -> Result<Self> {
		if data.len() < 3 {
			return Err(Error::FrameTooShort);
		}

		// 24-bit little-endian frame tag. Bit 0 is frame_type: 0 = key frame.
		let tag = u32::from(data[0]) | (u32::from(data[1]) << 8) | (u32::from(data[2]) << 16);
		let keyframe = tag & 0x1 == 0;

		if !keyframe {
			return Ok(Self {
				keyframe: false,
				dimensions: None,
			});
		}

		if data.len() < 10 {
			return Err(Error::KeyframeHeaderTooShort);
		}
		if !(data[3] == 0x9d && data[4] == 0x01 && data[5] == 0x2a) {
			return Err(Error::StartCodeMismatch);
		}

		// 14-bit dimensions; the top 2 bits of each field are the scaling factor.
		let width = (u16::from(data[6]) | (u16::from(data[7]) << 8)) & 0x3fff;
		let height = (u16::from(data[8]) | (u16::from(data[9]) << 8)) & 0x3fff;

		Ok(Self {
			keyframe: true,
			dimensions: Some((width, height)),
		})
	}
}

/// Build the informational `vpcC` configuration record for VP8.
///
/// VP8 is always 8-bit 4:2:0 with no out-of-band parameters, so the box carries
/// fixed placeholders. Profile 0 / level 0 are the standard "unspecified"
/// values. See https://www.webmproject.org/vp9/mp4/.
pub(crate) fn vpcc() -> mp4_atom::VpcC {
	mp4_atom::VpcC {
		profile: 0,
		level: 0,
		bit_depth: 8,
		..Default::default()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_keyframe_dimensions() {
		// Key frame tag (bit 0 = 0) + start code + 320x240.
		let frame = [0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x40, 0x01, 0xf0, 0x00];
		let header = FrameHeader::parse(&frame).expect("parse key frame");
		assert!(header.keyframe);
		assert_eq!(header.dimensions, Some((320, 240)));
	}

	#[test]
	fn parses_interframe() {
		// Interframe tag (bit 0 = 1); no start code or dimensions follow.
		let frame = [0x31, 0x00, 0x00];
		let header = FrameHeader::parse(&frame).expect("parse interframe");
		assert!(!header.keyframe);
		assert_eq!(header.dimensions, None);
	}

	#[test]
	fn rejects_bad_start_code() {
		let frame = [0x10, 0x00, 0x00, 0xde, 0xad, 0xbe, 0x40, 0x01, 0xf0, 0x00];
		assert!(FrameHeader::parse(&frame).is_err());
	}

	#[test]
	fn rejects_short_frame() {
		assert!(FrameHeader::parse(&[0x10, 0x00]).is_err());
	}
}
