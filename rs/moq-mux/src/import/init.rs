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
/// Each optional field applies to a subset of formats, and a format that cannot honor one rejects it
/// rather than dropping it. [`label`](Self::label) describes one rendition, so a container, which
/// publishes and describes its own tracks, refuses it. [`video`](Self::video) seeds a video config,
/// so an audio format, which reads its whole config from [`data`](Self::data), refuses that too.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Init {
	/// The media format, e.g. `"avc3"`, `"opus"`, `"aac"`, or a container like `"fmp4"`.
	pub format: String,
	/// Codec init bytes. Required for audio; may be empty for a video format that resolves in band.
	pub data: Bytes,
	/// Human-readable rendition name for a track picker. The single source: a label can never be
	/// detected from the stream, so nothing else sets one.
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

	/// Error if a field was set that this kind of format cannot honor.
	pub(crate) fn reject_unsupported(&self, kind: Kind) -> crate::Result<()> {
		let unsupported = |field| {
			Err(crate::Error::UnsupportedField {
				field,
				kind: kind.name(),
			})
		};

		match kind {
			// A container names and describes each track it publishes, so neither field has a single
			// rendition to land on.
			Kind::Container => {
				if self.label.is_some() {
					return unsupported("label");
				}
				if self.video.is_some() {
					return unsupported("video hint");
				}
			}
			// An audio importer builds its whole config from the init bytes; only the label rides along.
			Kind::Audio => {
				if self.video.is_some() {
					return unsupported("video hint");
				}
			}
			// Video honors everything.
			Kind::Video => {}
		}

		Ok(())
	}
}

/// What an [`Init`] format resolves to, which decides the fields it can honor.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Kind {
	Audio,
	Video,
	Container,
}

impl Kind {
	fn name(self) -> &'static str {
		match self {
			Kind::Audio => "audio",
			Kind::Video => "video",
			Kind::Container => "container",
		}
	}
}
