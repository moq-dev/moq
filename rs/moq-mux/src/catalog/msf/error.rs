//! Typed errors from the MSF catalog consumer.

/// Reasons an [`super::Consumer::poll_next`] can fail.
///
/// MSF catalog parsing flows through several layers (transport, JSON, base64,
/// ISO-BMFF, codec-specific config blobs) and any of them can surface a
/// malformed payload. Variants are grouped by the layer that produced the
/// failure, with the originating track name carried alongside when known.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Transport-layer error from the underlying track.
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Catalog frame was not valid UTF-8.
	#[error("catalog frame is not valid UTF-8: {0}")]
	Utf8(#[from] std::str::Utf8Error),

	/// Catalog JSON did not parse.
	#[error("catalog JSON parse failed: {0}")]
	Json(#[from] serde_json::Error),

	/// `init_data` bytes were not valid base64.
	#[error("track {track:?} has malformed init_data: {source}")]
	Base64 {
		track: String,
		#[source]
		source: base64::DecodeError,
	},

	/// CMAF init bytes did not decode as ISO-BMFF.
	#[error("CMAF track {track:?} init segment is malformed: {source}")]
	Mp4 {
		track: String,
		#[source]
		source: mp4_atom::Error,
	},

	/// Codec string was syntactically invalid.
	#[error("track {track:?} has invalid codec {codec:?}: {source}")]
	InvalidCodec {
		track: String,
		codec: String,
		#[source]
		source: hang::Error,
	},

	/// MSF catalog violates a schema invariant the consumer enforces (missing
	/// codec, CMAF without init_data, audio without samplerate / channelConfig
	/// and no init_data to derive from, etc.). `reason` is a static description
	/// of which invariant was violated.
	#[error("track {track:?}: {reason}")]
	Schema { track: String, reason: &'static str },

	/// Audio packaging is unsupported for parameter derivation (samplerate /
	/// channelConfig must be supplied explicitly for these tracks).
	#[error("audio track {track:?} packaging {packaging:?} is unsupported for parameter derivation")]
	UnsupportedAudioPackaging { track: String, packaging: String },

	/// Codec-specific audio config blob (AAC `AudioSpecificConfig`, Opus
	/// `OpusHead`, CMAF audio sample entry) failed to parse. `kind` tags which
	/// kind of config was being read; `detail` carries the parser's message.
	#[error("audio track {track:?}: {kind}: {detail}")]
	AudioConfig {
		track: String,
		kind: &'static str,
		detail: String,
	},
}
