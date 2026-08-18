use bytes::Bytes;

use crate::catalog::VideoHint;

/// What an audio importer needs to start: a format, its init bytes, and an optional label.
///
/// `format` selects the codec parser (`"opus"`, `"aac"`, `"flac"`, `"mp3"`). `data` carries the
/// codec init bytes (an OpusHead, an AudioSpecificConfig, ...), which audio needs up front because
/// an audio importer cannot resolve its config from frames.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct AudioInit {
	/// The audio format, e.g. `"opus"` or `"aac"`.
	pub format: String,
	/// Codec init bytes. Required: audio has no in-band config.
	pub data: Bytes,
	/// Human-readable rendition name for a track picker.
	pub label: Option<String>,
}

impl AudioInit {
	/// An init with just a format and its codec init bytes.
	pub fn new(format: impl Into<String>, data: impl Into<Bytes>) -> Self {
		Self {
			format: format.into(),
			data: data.into(),
			label: None,
		}
	}
}

/// What a video importer needs to start: a format, optional init bytes, a label, and hints.
///
/// `format` selects the codec parser (`"avc3"`, `"hev1"`, `"vp8"`, ...). `data` may be empty for a
/// format that resolves in band; a [`hint`](Self::hint) can pin fields the stream never reveals
/// (bitrate) or publish the catalog before the first keyframe. See [`VideoHint`].
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct VideoInit {
	/// The video format, e.g. `"avc3"` or `"vp8"`.
	pub format: String,
	/// Codec init bytes (an avcC, an hvcC, ...). May be empty for a format that resolves in band.
	pub data: Bytes,
	/// Human-readable rendition name for a track picker.
	pub label: Option<String>,
	/// Catalog fields the stream cannot reveal itself.
	pub hint: VideoHint,
}

impl VideoInit {
	/// An init with just a format and its codec init bytes (which may be empty).
	pub fn new(format: impl Into<String>, data: impl Into<Bytes>) -> Self {
		Self {
			format: format.into(),
			data: data.into(),
			label: None,
			hint: VideoHint::default(),
		}
	}
}

/// What a container importer needs to start: a format and its leading bytes.
///
/// A container publishes and describes its own tracks, so there is no label or hint here: a
/// rendition field would have no single track to land on.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct ContainerInit {
	/// The container format, e.g. `"fmp4"` or `"ts"`.
	pub format: String,
	/// The leading chunk of the container, decoded immediately. May be empty.
	pub data: Bytes,
}

impl ContainerInit {
	/// An init with just a format and its leading bytes.
	pub fn new(format: impl Into<String>, data: impl Into<Bytes>) -> Self {
		Self {
			format: format.into(),
			data: data.into(),
		}
	}
}

/// Which importer a format string selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// A single audio codec, imported by [`Track::audio`](super::Track::audio).
	Audio,
	/// A single video codec, imported by [`Track::video`](super::Track::video).
	Video,
	/// A container that publishes its own tracks, imported by [`Container`](super::Container).
	Container,
}

impl Kind {
	/// Which importer handles `format`, or `None` if none does.
	///
	/// For a caller that has a format string and has to pick an entry point, and for the error a
	/// constructor raises when handed a format belonging to a different kind.
	pub fn of(format: &str) -> Option<Self> {
		Some(match format {
			"aac" | "opus" | "flac" | "mp3" => Kind::Audio,
			"avc1" | "avcc" | "avc3" | "h264" | "hvc1" | "hvcc" | "hev1" | "av01" | "av1" | "av1c" | "av1C" | "vp8"
			| "vp08" | "vp9" | "vp09" => Kind::Video,
			"fmp4" | "cmaf" | "mkv" | "webm" | "matroska" | "ts" | "mpegts" | "mpeg2ts" | "m2ts" | "flv" => {
				Kind::Container
			}
			_ => return None,
		})
	}

	/// The name used in errors and docs.
	pub fn name(self) -> &'static str {
		match self {
			Kind::Audio => "audio",
			Kind::Video => "video",
			Kind::Container => "container",
		}
	}
}

/// The error for a format handed to the wrong constructor.
///
/// Falls back to [`UnknownFormat`](crate::Error::UnknownFormat) when nothing claims the format, so
/// a format this table has not caught up with degrades the message rather than misreporting it.
pub(crate) fn wrong_kind(format: &str, wanted: Kind) -> crate::Error {
	match Kind::of(format) {
		Some(actual) if actual != wanted => crate::Error::WrongKind {
			format: format.to_string(),
			actual: actual.name(),
			wanted: wanted.name(),
		},
		_ => crate::Error::UnknownFormat(format.to_string()),
	}
}
