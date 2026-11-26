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
	/// The container wrapping each frame on the wire.
	///
	/// [`Legacy`](hang::catalog::Container::Legacy) unless selected. A video rendition carries the
	/// same knob on its [`VideoInit::hint`].
	pub container: hang::catalog::Container,
	/// The maximum bitrate in bits per second, for an encoder that knows it. Supplied, it replaces
	/// any value the init bytes give and is never estimated.
	pub bitrate: Option<u64>,
}

impl AudioInit {
	/// An init with just a format and its codec init bytes.
	pub fn new(format: AudioFormat, data: impl Into<Bytes>) -> Self {
		Self {
			format,
			data: data.into(),
			label: None,
			container: hang::catalog::Container::default(),
			bitrate: None,
		}
	}
}

/// What a video importer needs to start: a format, optional init bytes, a label, and hints.
///
/// `format` selects the codec parser (`"h264"`, `"h265"`, `"vp8"`, ...). `data` may be empty for a
/// format that resolves in band; a [`hint`](Self::hint) can pin fields the stream never reveals
/// (bitrate) or publish the catalog before the first keyframe. See [`VideoHint`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct VideoInit {
	/// The video format, e.g. `"h264"` or `"vp8"`.
	pub format: VideoFormat,
	/// Codec init bytes (an avcC, an hvcC, ...). May be empty for a format that resolves in band.
	pub data: Bytes,
	/// Human-readable rendition name for a track picker.
	pub label: Option<String>,
	/// Catalog fields the stream cannot reveal itself.
	pub hint: VideoHint,
	/// The track's priority, which a [`bandwidth::Allocator`](moq_net::bandwidth::Allocator)
	/// funds in order. A rendition published above a lower one of the same source sits one
	/// below it, so the lower one is funded first. Defaults to
	/// [`PRIORITY.video`](hang::catalog::PRIORITY).
	pub priority: i8,
}

impl VideoInit {
	/// An init with just a format and its codec init bytes (which may be empty).
	pub fn new(format: VideoFormat, data: impl Into<Bytes>) -> Self {
		Self {
			format,
			data: data.into(),
			label: None,
			hint: VideoHint::default(),
			priority: hang::catalog::PRIORITY.video,
		}
	}
}

/// What a container importer needs to start: a format, its leading bytes, and an optional shared
/// timebase.
///
/// A container publishes and describes its own tracks, so there is no label or hint here: a
/// rendition field would have no single track to land on.
#[non_exhaustive]
pub struct ContainerInit<E: crate::catalog::hang::CatalogExt = ()> {
	/// The container format, e.g. `"fmp4"` or `"ts"`.
	pub format: ContainerFormat,
	/// The leading chunk of the container, decoded immediately. May be empty.
	pub data: Bytes,
	/// The timestamp base the container shifts through, shared with other importers of its source,
	/// or `None` for a fresh one of its own.
	pub timebase: Option<crate::catalog::Timebase<E>>,
}

impl<E: crate::catalog::hang::CatalogExt> ContainerInit<E> {
	/// An init with just a format and its leading bytes, on a timebase of its own.
	pub fn new(format: ContainerFormat, data: impl Into<Bytes>) -> Self {
		Self {
			format,
			data: data.into(),
			timebase: None,
		}
	}

	/// Shift through `timebase`, shared with other importers of the same source.
	pub fn with_timebase(mut self, timebase: impl Into<Option<crate::catalog::Timebase<E>>>) -> Self {
		self.timebase = timebase.into();
		self
	}
}

// Manual so an init is clonable and printable regardless of whether `E` is.
impl<E: crate::catalog::hang::CatalogExt> Clone for ContainerInit<E> {
	fn clone(&self) -> Self {
		Self {
			format: self.format,
			data: self.data.clone(),
			timebase: self.timebase.clone(),
		}
	}
}

impl<E: crate::catalog::hang::CatalogExt> std::fmt::Debug for ContainerInit<E> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ContainerInit")
			.field("format", &self.format)
			.field("data", &self.data)
			.field("shared", &self.timebase.is_some())
			.finish()
	}
}
