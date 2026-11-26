//! Typed media formats.
//!
//! A format selects a parser, so unlike a WebCodecs codec string (`avc1.64001f`) it is a closed
//! set. Each import entry point takes the format type for its kind, so a format belonging to
//! another kind cannot be passed at all; the only place the two can meet is [`FromStr`], which is
//! where a caller crossing a string boundary (a CLI flag, an FFI call, a container's codec tag)
//! finds out.

use std::fmt;
use std::str::FromStr;

/// A single audio codec an importer can parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AudioFormat {
	/// Advanced Audio Coding, configured by an AudioSpecificConfig.
	Aac,
	/// Opus, configured by an OpusHead.
	Opus,
	/// FLAC, configured by the `fLaC` marker plus its STREAMINFO block.
	Flac,
	/// MPEG-1/2 Audio Layer III.
	Mp3,
}

/// A single video codec an importer can parse.
///
/// H.264 and H.265 each appear twice because the input framing differs: `avc1`/`hvc1` are
/// length-prefixed NAL units with an out-of-band config record, while `h264`/`h265` are a raw Annex B
/// elementary stream split on start codes, carrying its parameter sets inline. The raw names follow
/// ffmpeg's `h264`/`hevc` formats rather than `avc3`/`hev1`, which say where parameter sets live, not
/// how NAL units are framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VideoFormat {
	/// H.264, length-prefixed NALUs with an out-of-band avcC.
	Avc1,
	/// H.264, a raw Annex B elementary stream with inline SPS/PPS.
	H264,
	/// H.265, length-prefixed NALUs with an out-of-band hvcC.
	Hvc1,
	/// H.265, a raw Annex B elementary stream with inline parameter sets.
	H265,
	/// AV1.
	Av01,
	/// VP8.
	Vp8,
	/// VP9.
	Vp9,
}

/// A container an importer can demux, which may publish more than one track.
///
/// Names only the input the importer parses. Every container is demuxed into its tracks' frames;
/// republishing the input whole is a choice made on the concrete importer
/// ([`fmp4::Import::with_cmaf`](crate::container::fmp4::Import::with_cmaf),
/// [`ts::Passthrough`](crate::container::ts::Passthrough)), never by the format name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ContainerFormat {
	/// Fragmented MP4 / CMAF.
	Fmp4,
	/// Regular MP4, with its `moov` before or after its media, or fragmented.
	Mp4,
	/// Matroska / WebM.
	Mkv,
	/// MPEG-2 transport stream.
	Ts,
	/// Flash Video, as used by RTMP.
	Flv,
}

/// Any format an importer handles, and the kind of import it selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
	/// A single audio codec.
	Audio(AudioFormat),
	/// A single video codec.
	Video(VideoFormat),
	/// A container publishing its own tracks.
	Container(ContainerFormat),
}

/// Which importer a [`Format`] selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
	/// A single audio codec, imported by [`Track::audio`](crate::import::Track::audio).
	Audio,
	/// A single video codec, imported by [`Track::video`](crate::import::Track::video).
	Video,
	/// A container that publishes its own tracks, imported by [`Container`](crate::import::Container).
	Container,
}

impl Kind {
	/// The name used in errors and docs.
	pub fn name(self) -> &'static str {
		match self {
			Kind::Audio => "audio",
			Kind::Video => "video",
			Kind::Container => "container",
		}
	}
}

impl Format {
	/// Which import entry point handles this format.
	pub fn kind(self) -> Kind {
		match self {
			Format::Audio(_) => Kind::Audio,
			Format::Video(_) => Kind::Video,
			Format::Container(_) => Kind::Container,
		}
	}
}

impl FromStr for Format {
	type Err = crate::Error;

	/// Parse a format name, accepting each format's aliases.
	///
	/// The one table: every other `FromStr` here defers to it, so an alias is written down once and
	/// a name can never mean different things to the classifier and the importer.
	fn from_str(s: &str) -> Result<Self, Self::Err> {
		Ok(match s {
			"aac" => Format::Audio(AudioFormat::Aac),
			"opus" => Format::Audio(AudioFormat::Opus),
			"flac" => Format::Audio(AudioFormat::Flac),
			"mp3" => Format::Audio(AudioFormat::Mp3),

			"avc1" | "avcc" => Format::Video(VideoFormat::Avc1),
			"h264" => Format::Video(VideoFormat::H264),
			"hvc1" | "hvcc" => Format::Video(VideoFormat::Hvc1),
			"h265" | "hevc" => Format::Video(VideoFormat::H265),
			"av01" | "av1" | "av1c" | "av1C" => Format::Video(VideoFormat::Av01),
			"vp8" | "vp08" => Format::Video(VideoFormat::Vp8),
			"vp9" | "vp09" => Format::Video(VideoFormat::Vp9),

			"fmp4" | "cmaf" => Format::Container(ContainerFormat::Fmp4),
			"mp4" => Format::Container(ContainerFormat::Mp4),
			"mkv" | "webm" | "matroska" => Format::Container(ContainerFormat::Mkv),
			// Not `m2ts`, which names BDAV's 192-byte packets rather than 188-byte TS.
			"ts" | "mpegts" | "mpeg2ts" => Format::Container(ContainerFormat::Ts),
			"flv" => Format::Container(ContainerFormat::Flv),

			_ => return Err(crate::Error::UnknownFormat(s.to_string())),
		})
	}
}

impl fmt::Display for Format {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Format::Audio(format) => format.fmt(f),
			Format::Video(format) => format.fmt(f),
			Format::Container(format) => format.fmt(f),
		}
	}
}

/// Parse `s` as a format of one kind, reporting the kind that does handle it otherwise.
///
/// A caller that reached the wrong entry point learns which one to use, rather than being told the
/// format is unknown when it is perfectly well known.
fn parse_kind(s: &str, wanted: Kind) -> Result<Format, crate::Error> {
	let format: Format = s.parse()?;
	if format.kind() != wanted {
		return Err(crate::Error::WrongKind {
			format: s.to_string(),
			actual: format.kind().name(),
			wanted: wanted.name(),
		});
	}
	Ok(format)
}

macro_rules! kind_format {
	($ty:ident, $kind:ident, $($variant:ident => $name:literal),+ $(,)?) => {
		impl fmt::Display for $ty {
			fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
				f.write_str(match self {
					$($ty::$variant => $name,)+
				})
			}
		}

		impl FromStr for $ty {
			type Err = crate::Error;

			/// Parse a format name of this kind, accepting its aliases. A name belonging to another
			/// kind reports [`WrongKind`](crate::Error::WrongKind) rather than being unknown.
			fn from_str(s: &str) -> Result<Self, Self::Err> {
				match parse_kind(s, Kind::$kind)? {
					Format::$kind(format) => Ok(format),
					// `parse_kind` already rejected every other kind.
					_ => unreachable!(),
				}
			}
		}
	};
}

kind_format!(AudioFormat, Audio,
	Aac => "aac", Opus => "opus", Flac => "flac", Mp3 => "mp3");

kind_format!(VideoFormat, Video,
	Avc1 => "avc1", H264 => "h264", Hvc1 => "hvc1", H265 => "h265",
	Av01 => "av01", Vp8 => "vp8", Vp9 => "vp9");

kind_format!(ContainerFormat, Container,
	Fmp4 => "fmp4", Mp4 => "mp4", Mkv => "mkv", Ts => "ts", Flv => "flv");

impl AudioFormat {
	/// The suffix of a track named after this format, such as `.opus` in `0.opus`.
	pub fn track_suffix(self) -> &'static str {
		match self {
			AudioFormat::Aac => ".aac",
			AudioFormat::Opus => ".opus",
			AudioFormat::Flac => ".flac",
			AudioFormat::Mp3 => ".mp3",
		}
	}
}

impl VideoFormat {
	/// The suffix of a track named after this format, such as `.avc3` in `0.avc3`.
	///
	/// Names the codec family the track publishes rather than the input, so a raw Annex B stream
	/// is `.avc3` or `.hev1` like every other in-band H.26x track.
	pub fn track_suffix(self) -> &'static str {
		match self {
			VideoFormat::Avc1 => ".avc1",
			VideoFormat::H264 => ".avc3",
			VideoFormat::Hvc1 => ".hvc1",
			VideoFormat::H265 => ".hev1",
			VideoFormat::Av01 => ".av01",
			VideoFormat::Vp8 => ".vp8",
			VideoFormat::Vp9 => ".vp9",
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Every alias resolves, and `Display` gives back the canonical name rather than the alias, so a
	/// track named after its format is named the same whichever spelling the caller used.
	#[test]
	fn aliases_resolve_to_one_canonical_name() {
		for (aliases, canonical) in [
			(vec!["h264"], "h264"),
			(vec!["h265", "hevc"], "h265"),
			(vec!["avc1", "avcc"], "avc1"),
			(vec!["hvc1", "hvcc"], "hvc1"),
			(vec!["av01", "av1", "av1c", "av1C"], "av01"),
			(vec!["vp8", "vp08"], "vp8"),
			(vec!["vp9", "vp09"], "vp9"),
			(vec!["fmp4", "cmaf"], "fmp4"),
			(vec!["mp4"], "mp4"),
			(vec!["mkv", "webm", "matroska"], "mkv"),
			(vec!["ts", "mpegts", "mpeg2ts"], "ts"),
			(vec!["opus"], "opus"),
		] {
			for alias in aliases {
				let format: Format = alias.parse().unwrap_or_else(|err| panic!("{alias}: {err}"));
				assert_eq!(format.to_string(), canonical, "{alias} should canonicalize");
			}
		}
	}

	/// The whole point of the typed formats: a name of the wrong kind is caught at the string
	/// boundary, and says which entry point does take it.
	#[test]
	fn a_format_of_another_kind_names_the_kind_that_takes_it() {
		let err = "h264".parse::<AudioFormat>().unwrap_err();
		assert!(
			matches!(
				&err,
				crate::Error::WrongKind {
					actual: "video",
					wanted: "audio",
					..
				}
			),
			"got {err:?}"
		);

		let err = "fmp4".parse::<VideoFormat>().unwrap_err();
		assert!(
			matches!(
				&err,
				crate::Error::WrongKind {
					actual: "container",
					wanted: "video",
					..
				}
			),
			"got {err:?}"
		);

		let err = "opus".parse::<ContainerFormat>().unwrap_err();
		assert!(
			matches!(
				&err,
				crate::Error::WrongKind {
					actual: "audio",
					wanted: "container",
					..
				}
			),
			"got {err:?}"
		);
	}

	#[test]
	fn an_unrecognized_name_is_unknown_rather_than_wrong_kind() {
		let err = "prores".parse::<VideoFormat>().unwrap_err();
		assert!(
			matches!(&err, crate::Error::UnknownFormat(f) if f == "prores"),
			"got {err:?}"
		);
	}
}
