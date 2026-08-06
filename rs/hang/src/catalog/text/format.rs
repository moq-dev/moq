use crate::Error;

use derive_more::Display;
use std::str::FromStr;

/// The serialization format of a text track's frames.
///
/// The frame timestamp always carries the cue's start time (so the media-agnostic relay never
/// parses the payload); the payload carries the cue itself in this format.
#[derive(Debug, Clone, PartialEq, Eq, Display, Default)]
#[non_exhaustive]
pub enum TextFormat {
	/// [WebVTT]. Each frame's payload is a self-contained `WEBVTT` segment whose cues carry absolute
	/// timing, so a consumer can render it without prior context.
	///
	/// [WebVTT]: https://www.w3.org/TR/webvtt1/
	#[display("vtt")]
	#[default]
	Vtt,

	/// TTML / IMSC1 ([W3C IMSC]) fragment (XML). Each frame's payload is a document fragment with
	/// absolute timing.
	///
	/// [W3C IMSC]: https://www.w3.org/TR/ttml-imsc1.1/
	#[display("ttml")]
	Ttml,

	/// Raw UTF-8 text with no embedded timing or styling. Each cue is shown from its frame
	/// timestamp until the next cue (an empty payload clears the caption), i.e. roll-up captions.
	#[display("utf8")]
	Utf8,

	/// An unknown or unsupported format string, preserved verbatim so a newer format survives an
	/// older consumer.
	#[display("{_0}")]
	Unknown(String),
}

impl FromStr for TextFormat {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"vtt" => Self::Vtt,
			"ttml" => Self::Ttml,
			"utf8" => Self::Utf8,
			_ => Self::Unknown(s.to_string()),
		})
	}
}

/// The accessibility role of a text track, mirroring the roles registered by
/// [draft-ietf-moq-msf](https://datatracker.ietf.org/doc/draft-ietf-moq-msf/).
#[derive(Debug, Clone, PartialEq, Eq, Display, Default)]
#[non_exhaustive]
pub enum TextRole {
	/// A transcription of the spoken dialogue, same-language or translated (the common subtitle).
	#[display("subtitle")]
	#[default]
	Subtitle,

	/// A textual representation of all audio, including non-speech sounds, for viewers who cannot
	/// hear it (subtitles for the deaf and hard-of-hearing).
	#[display("caption")]
	Caption,

	/// An unknown or unsupported role string, preserved verbatim so a newer role survives an older
	/// consumer. The MSF registry it borrows from is expected to grow, and a rendition we can't
	/// classify is still worth listing in a picker.
	#[display("{_0}")]
	Unknown(String),
}

impl FromStr for TextRole {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"subtitle" => Self::Subtitle,
			"caption" => Self::Caption,
			_ => Self::Unknown(s.to_string()),
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn format_roundtrip() {
		for (s, f) in [
			("vtt", TextFormat::Vtt),
			("ttml", TextFormat::Ttml),
			("utf8", TextFormat::Utf8),
		] {
			assert_eq!(TextFormat::from_str(s).unwrap(), f);
			assert_eq!(f.to_string(), s);
		}
	}

	#[test]
	fn format_unknown_preserved() {
		let parsed = TextFormat::from_str("cea708").unwrap();
		assert_eq!(parsed, TextFormat::Unknown("cea708".to_string()));
		assert_eq!(parsed.to_string(), "cea708");
	}

	#[test]
	fn role_default_is_subtitle() {
		assert_eq!(TextRole::default(), TextRole::Subtitle);
	}

	#[test]
	fn role_roundtrip() {
		for (s, r) in [("subtitle", TextRole::Subtitle), ("caption", TextRole::Caption)] {
			assert_eq!(TextRole::from_str(s).unwrap(), r);
			assert_eq!(r.to_string(), s);
		}
	}

	#[test]
	fn role_unknown_preserved() {
		let parsed = TextRole::from_str("commentary").unwrap();
		assert_eq!(parsed, TextRole::Unknown("commentary".to_string()));
		assert_eq!(parsed.to_string(), "commentary");
	}
}
