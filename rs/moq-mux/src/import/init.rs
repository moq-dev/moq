use bytes::Bytes;

use crate::catalog::VideoHint;

/// What a single-track importer needs to start: a format, its init bytes, an optional label, and
/// optional video fields.
///
/// `format` selects the codec parser (e.g. `"avc3"`, `"opus"`). `data` carries the codec init bytes
/// (an avcC record, an OpusHead, an AudioSpecificConfig, ...). Audio formats need those bytes up
/// front (an audio importer can't resolve its config from frames); video formats may resolve lazily
/// from the stream, and a [`video`](Self::video) hint can pin fields the stream can't reveal
/// (bitrate) or publish the catalog before the first keyframe. See [`VideoHint`].
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Init {
	/// The media format, e.g. `"avc3"`, `"opus"`, or `"aac"`.
	pub format: String,
	/// Codec init bytes. Required for audio; may be empty for a video format that resolves in band.
	pub data: Bytes,
	/// Human-readable rendition name for a single-codec track picker entry.
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

	/// Attach a human-readable rendition name.
	pub fn with_label(mut self, label: impl Into<String>) -> Self {
		self.label = Some(label.into());
		self
	}

	/// Attach caller-provided video catalog fields.
	pub fn with_video(mut self, hint: VideoHint) -> Self {
		self.video = Some(hint);
		self
	}
}
