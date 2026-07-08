//! Codecs.
//!
//! One submodule per codec. Each owns parsers and builders for the
//! codec's configuration record (avcC, hvcC, av1C, AudioSpecificConfig,
//! OpusHead), any inline-to-out-of-band transforms applicable to that
//! codec, and an `Import` type that publishes a raw bitstream as a moq
//! broadcast.

pub mod aac;
pub(crate) mod ac3;
pub mod annexb;
pub mod av1;
pub(crate) mod eac3;
pub mod flac;
pub mod h264;
pub mod h265;
pub(crate) mod legacy;
pub(crate) mod mp2;
pub mod mp3;
pub mod opus;
pub mod vp8;
pub mod vp9;

/// Resolve an audio config for a single-track importer: use what it parsed from the init bytes, or
/// fall back to building one from the caller's [`AudioHint`](crate::catalog::AudioHint) alone.
///
/// Errors with [`MissingInit`](crate::Error::MissingInit) when there are neither init bytes nor
/// enough hint fields (codec, sample rate, channel count) to publish.
pub(crate) fn resolve_audio(
	format: &str,
	detected: Option<hang::catalog::AudioConfig>,
	hint: &crate::catalog::AudioHint,
) -> crate::Result<hang::catalog::AudioConfig> {
	match detected {
		Some(config) => Ok(config),
		None => hint.to_config()?.ok_or_else(|| crate::Error::MissingInit {
			format: format.to_string(),
			field: "codec/sample_rate/channel_count",
		}),
	}
}
