use bytes::Bytes;

use super::{AudioFormat, ContainerFormat, VideoFormat};
use crate::catalog::VideoHint;

/// What an audio importer needs to start: a format, its init bytes, and an optional label.
///
/// `format` selects the codec parser (`"opus"`, `"aac"`, `"flac"`, `"mp3"`). `data` carries the
/// codec init bytes (an OpusHead, an AudioSpecificConfig, ...), which audio needs up front because
/// an audio importer cannot resolve its config from frames.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AudioInit {
	/// The audio format, e.g. `"opus"` or `"aac"`.
	pub format: AudioFormat,
	/// Codec init bytes. Required: audio has no in-band config.
	pub data: Bytes,
	/// Human-readable rendition name for a track picker.
	pub label: Option<String>,
}

impl AudioInit {
	/// An init with just a format and its codec init bytes.
	pub fn new(format: AudioFormat, data: impl Into<Bytes>) -> Self {
		Self {
			format,
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
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct VideoInit {
	/// The video format, e.g. `"avc3"` or `"vp8"`.
	pub format: VideoFormat,
	/// Codec init bytes (an avcC, an hvcC, ...). May be empty for a format that resolves in band.
	pub data: Bytes,
	/// Human-readable rendition name for a track picker.
	pub label: Option<String>,
	/// Catalog fields the stream cannot reveal itself.
	pub hint: VideoHint,
}

impl VideoInit {
	/// An init with just a format and its codec init bytes (which may be empty).
	pub fn new(format: VideoFormat, data: impl Into<Bytes>) -> Self {
		Self {
			format,
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
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ContainerInit {
	/// The container format, e.g. `"fmp4"` or `"ts"`.
	pub format: ContainerFormat,
	/// The leading chunk of the container, decoded immediately. May be empty.
	pub data: Bytes,
}

impl ContainerInit {
	/// An init with just a format and its leading bytes.
	pub fn new(format: ContainerFormat, data: impl Into<Bytes>) -> Self {
		Self {
			format,
			data: data.into(),
		}
	}
}
