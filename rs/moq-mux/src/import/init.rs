use bytes::Bytes;

use crate::catalog::{AudioHint, VideoHint};

/// What a single-track importer needs to start: a format, its init bytes, and optional catalog fields.
///
/// `format` selects the codec parser (e.g. `"avc3"`, `"opus"`). `data` carries the usual init bytes
/// (an avcC record, an OpusHead, ...) when the caller has them. The [`audio`](Self::audio) /
/// [`video`](Self::video) hints let the caller pin catalog fields the stream can't reveal (bitrate,
/// language) or publish the catalog before the first frame; see [`AudioHint`] / [`VideoHint`].
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Init {
	/// The media format, e.g. `"avc3"`, `"opus"`, or `"aac"`.
	pub format: String,
	/// Codec init bytes, empty when the caller relies on a hint or in-band config.
	pub data: Bytes,
	/// Caller-provided fields for an audio track.
	pub audio: Option<AudioHint>,
	/// Caller-provided fields for a video track.
	pub video: Option<VideoHint>,
}

impl Init {
	/// An init with just a format and its bytes (either may be empty).
	pub fn new(format: impl Into<String>, data: impl Into<Bytes>) -> Self {
		Self {
			format: format.into(),
			data: data.into(),
			audio: None,
			video: None,
		}
	}

	/// Attach caller-provided audio catalog fields.
	pub fn with_audio(mut self, hint: AudioHint) -> Self {
		self.audio = Some(hint);
		self
	}

	/// Attach caller-provided video catalog fields.
	pub fn with_video(mut self, hint: VideoHint) -> Self {
		self.video = Some(hint);
		self
	}
}
