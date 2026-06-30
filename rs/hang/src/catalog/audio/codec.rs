use crate::Error;

use super::*;
use derive_more::{Display, From};
use std::str::FromStr;

/// Supported audio codec mimetypes.
#[derive(Debug, Clone, PartialEq, Eq, Display, From)]
#[non_exhaustive]
pub enum AudioCodec {
	/// AAC codec with profile information
	AAC(AAC),

	/// Opus codec (no mimetype parameters)
	#[display("opus")]
	Opus,

	/// FLAC, the Free Lossless Audio Codec (RFC 9639). The decoder
	/// initialization data (the `fLaC` stream marker plus the STREAMINFO
	/// metadata block) travels out of band in [`AudioConfig::description`]. Both
	/// the codec string and the description match the WebCodecs FLAC
	/// registration, so browsers decode it directly.
	///
	/// [`AudioConfig::description`]: super::AudioConfig::description
	#[display("flac")]
	Flac,

	/// MPEG-1/2 Audio Layer III (MP3). Configuration is carried in band in each
	/// frame header, so there is no out-of-band description. Browsers decode it
	/// via WebCodecs (`AudioDecoder` codec string `"mp3"`), so unlike the legacy
	/// codecs below it gets its own [`AudioCodecKind`].
	#[display("mp3")]
	Mp3,

	/// MPEG-1/2 Audio Layer II. Legacy broadcast codec, carried verbatim by the
	/// MPEG-TS bridge for TS gear. WebCodecs cannot decode it, so browsers should
	/// skip this rendition. Do not use it for new content.
	#[display("mp2")]
	Mp2,

	/// Dolby Digital (AC-3). Legacy broadcast codec, same contract as
	/// [`Self::Mp2`]: TS bridge only, not decodable in browsers, not for new
	/// content.
	#[display("ac-3")]
	Ac3,

	/// Dolby Digital Plus, also known as E-AC-3 or Enhanced AC-3 ("ec-3" is its
	/// registered codec string). Legacy broadcast codec, same contract as
	/// [`Self::Mp2`]: TS bridge only, not decodable in browsers, not for new
	/// content.
	#[display("ec-3")]
	Ec3,

	/// Unknown or unsupported codec with original string
	#[display("{_0}")]
	Unknown(String),
}

/// Coarse audio codec family, used for tag-only matching.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioCodecKind {
	AAC,
	Opus,
	Flac,
	Mp3,
	Unknown,
}

impl AudioCodec {
	/// Return the coarse codec family for tag-only matching.
	pub fn kind(&self) -> AudioCodecKind {
		match self {
			Self::AAC(_) => AudioCodecKind::AAC,
			Self::Opus => AudioCodecKind::Opus,
			Self::Flac => AudioCodecKind::Flac,
			Self::Mp3 => AudioCodecKind::Mp3,
			// Legacy TS-bridge codecs aren't WebCodecs-decodable, so they share the
			// coarse Unknown family for tag-only matching.
			Self::Mp2 | Self::Ac3 | Self::Ec3 => AudioCodecKind::Unknown,
			Self::Unknown(_) => AudioCodecKind::Unknown,
		}
	}
}

impl FromStr for AudioCodec {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		if s.starts_with("mp4a.40.") {
			return AAC::from_str(s).map(Into::into);
		} else if s == "opus" {
			return Ok(Self::Opus);
		} else if s == "flac" {
			return Ok(Self::Flac);
		} else if s == "mp3" {
			return Ok(Self::Mp3);
		} else if s == "mp2" {
			return Ok(Self::Mp2);
		} else if s == "ac-3" {
			return Ok(Self::Ac3);
		} else if s == "ec-3" {
			return Ok(Self::Ec3);
		}

		Ok(Self::Unknown(s.to_string()))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn flac_roundtrip() {
		let codec = AudioCodec::from_str("flac").unwrap();
		assert_eq!(codec, AudioCodec::Flac);
		assert_eq!(codec.to_string(), "flac");
		assert_eq!(codec.kind(), AudioCodecKind::Flac);
	}
}
