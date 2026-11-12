use serde::{Deserialize, Serialize};
use std::fmt;

use crate::vendor::str0m::packet::MediaKind;
use crate::vendor::str0m::rtp_::Frequency;

use super::format_params::FormatParams;

/// Codec specification
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecSpec {
	/// The codec identifier.
	pub codec: Codec,

	/// Clock rate of the codec.
	pub clock_rate: Frequency,

	/// Number of audio channels (if any).
	pub channels: Option<u8>,

	/// Codec specific format parameters. This might carry additional config for
	/// things like h264.
	pub format: FormatParams,
}

/// Known codecs and RTP payload formats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
#[allow(missing_docs)]
pub enum Codec {
	Opus,
	AmrWb,
	H264,
	// TODO show this when we support h265.
	H265,
	Vp8,
	Vp9,
	// TODO show this when we support Av1.
	Av1,
	/// Technically not a codec, but used in places where codecs go
	/// in `a=rtpmap` lines.
	Rtx,
	/// For RTP mode. No codec.
	Null,
	Unknown,
}

impl Codec {
	/// Tells if codec is audio.
	pub fn is_audio(&self) -> bool {
		use Codec::*;
		matches!(self, Opus | AmrWb)
	}

	/// Tells if codec is video.
	pub fn is_video(&self) -> bool {
		use Codec::*;
		matches!(self, H265 | H264 | Vp8 | Vp9 | Av1)
	}

	/// Audio/Video.
	pub fn kind(&self) -> MediaKind {
		if self.is_audio() {
			MediaKind::Audio
		} else {
			MediaKind::Video
		}
	}
}

impl<'a> From<&'a str> for Codec {
	fn from(v: &'a str) -> Self {
		let lc = v.to_ascii_lowercase();
		match &lc[..] {
			"opus" => Codec::Opus,
			"amr-wb" => Codec::AmrWb,
			"h264" => Codec::H264,
			"h265" => Codec::H265,
			"vp8" => Codec::Vp8,
			"vp9" => Codec::Vp9,
			"av1" => Codec::Av1,
			"rtx" => Codec::Rtx, // resends
			_ => Codec::Unknown,
		}
	}
}

impl fmt::Display for Codec {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Codec::Opus => write!(f, "opus"),
			Codec::AmrWb => write!(f, "AMR-WB"),
			Codec::H264 => write!(f, "H264"),
			Codec::H265 => write!(f, "H265"),
			Codec::Vp8 => write!(f, "VP8"),
			Codec::Vp9 => write!(f, "VP9"),
			Codec::Av1 => write!(f, "AV1"),
			Codec::Rtx => write!(f, "rtx"),
			Codec::Null => write!(f, "null"),
			Codec::Unknown => write!(f, "unknown"),
		}
	}
}
