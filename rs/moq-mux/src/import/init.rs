use bytes::Bytes;

use crate::catalog::VideoHint;

/// What a media importer needs to start: a format, its init bytes, an optional label, and optional
/// video fields.
///
/// `format` selects either a codec parser (e.g. `"avc3"`, `"opus"`) or a container
/// (e.g. `"fmp4"`, `"ts"`). `data` carries the codec init bytes (an avcC record, an OpusHead, an
/// AudioSpecificConfig, ...) or the container's leading chunk. Audio formats need those bytes up
/// front (an audio importer can't resolve its config from frames); video formats may resolve lazily
/// from the stream, and a [`video`](Self::video) hint can pin fields the stream can't reveal
/// (bitrate) or publish the catalog before the first keyframe. See [`VideoHint`].
///
/// [`label`](Self::label) and [`video`](Self::video) describe one rendition, so they apply only to a
/// codec format. A container publishes its own tracks and describes each from its own metadata:
/// [`Container::new`](crate::import::Container::new) rejects either field rather than dropping it.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Init {
	/// The media format, e.g. `"avc3"`, `"opus"`, `"aac"`, or a container like `"fmp4"`.
	pub format: String,
	/// Codec init bytes. Required for audio; may be empty for a video format that resolves in band.
	pub data: Bytes,
	/// Human-readable rendition name for a track picker.
	///
	/// For a video format this is the default for [`VideoHint::label`]: an explicit hint label wins,
	/// so set one or the other rather than both.
	pub label: Option<String>,
	/// Caller-provided fields for a video track.
	pub video: Option<VideoHint>,
}

impl Init {
	/// An init with just a format and its bytes (data may be empty for a lazy video format).
	pub fn new(format: impl Into<String>, data: impl Into<Bytes>) -> Self {
		Self {
			format: format.into(),
			data: data.into(),
			label: None,
			video: None,
		}
	}

	/// Attach caller-provided video catalog fields.
	pub fn with_video(mut self, hint: VideoHint) -> Self {
		self.video = Some(hint);
		self
	}

	/// Error if a field describing a single rendition was set, for a container that has many.
	pub(crate) fn reject_container_fields(&self) -> crate::Result<()> {
		if self.label.is_some() {
			return Err(crate::Error::UnsupportedByContainer("label"));
		}
		if self.video.is_some() {
			return Err(crate::Error::UnsupportedByContainer("video hint"));
		}
		Ok(())
	}
}
