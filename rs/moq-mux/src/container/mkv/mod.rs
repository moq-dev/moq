//! Matroska / WebM.
//!
//! An EBML-based file format. moq-mux uses it as an external interchange
//! format only, not as a wire format: [`Import`] parses MKV byte streams
//! into a broadcast and [`Export`] does the reverse.

mod export;
mod import;

pub use export::*;
pub use import::*;

#[cfg(test)]
mod export_test;
#[cfg(test)]
mod import_test;

/// MKV parsing and emission errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("unsupported EBML DocType: {0}")]
	UnsupportedDocType(String),

	#[error("EBML header missing DocType")]
	MissingDocType,

	#[error("invalid SimpleBlock")]
	InvalidSimpleBlock,

	#[error("invalid Block payload")]
	InvalidBlock,

	#[error("negative block timestamp")]
	NegativeBlockTimestamp,

	#[error("timestamp overflow")]
	TimestampOverflow,

	#[error("TrackEntry missing TrackNumber")]
	MissingTrackNumber,

	#[error("TrackEntry missing TrackType")]
	MissingTrackType,

	#[error("TrackEntry missing CodecID")]
	MissingCodecId,

	#[error("unsupported video CodecID: {0}")]
	UnsupportedVideoCodec(String),

	#[error("unsupported audio CodecID: {0}")]
	UnsupportedAudioCodec(String),

	#[error("{codec_id} missing CodecPrivate ({purpose})")]
	MissingCodecPrivate {
		codec_id: &'static str,
		purpose: &'static str,
	},

	#[error("invalid HEVCDecoderConfigurationRecord")]
	InvalidHvcc,

	#[error("invalid AV1CodecConfigurationRecord")]
	InvalidAv1c,

	#[error("MKV track layout changed after header was emitted: track '{0}' added")]
	HeaderAddedTrack(String),

	#[error("MKV track layout changed after header was emitted: track '{0}' removed")]
	HeaderRemovedTrack(String),

	#[error("MKV export does not support CMAF {kind} track '{name}'")]
	UnsupportedCmafTrack { kind: String, name: String },

	#[error("MKV export does not support video codec {0}")]
	UnsupportedVideoExport(String),

	#[error("MKV export does not support audio codec {0}")]
	UnsupportedAudioExport(String),

	#[error("AAC track missing AudioSpecificConfig (description)")]
	MissingAacDescription,

	#[error("FLAC track missing STREAMINFO (description)")]
	MissingFlacDescription,

	#[error("H.264 track missing AVCDecoderConfigurationRecord")]
	MissingH264Avcc,

	#[error("H.265 track missing HEVCDecoderConfigurationRecord")]
	MissingH265Hvcc,

	#[error("cluster underflow")]
	ClusterUnderflow,

	#[error("block timestamp doesn't fit in i16")]
	BlockTimestampOverflow,

	#[error("missing track")]
	MissingTrack,

	#[error("timestamp doesn't fit in u64 ms")]
	TimestampU64,

	#[error("video track {0} missing in tracks map")]
	MissingVideoTrack(String),

	#[error("audio track {0} missing in tracks map")]
	MissingAudioTrack(String),

	#[error("no catalog snapshot")]
	NoCatalogSnapshot,

	#[error("matroska parse error")]
	MatroskaParse,

	#[error("matroska write error: {0}")]
	MatroskaWrite(String),

	/// Building the Opus codec-private OpusHead for an audio track failed.
	#[error(transparent)]
	Opus(#[from] crate::codec::opus::Error),
}

// Flattened to its message so webm-iterable stays out of this crate's public API. The chain
// matters here: `TagWriterError::WriteError` displays only "Error writing to destination." and
// keeps the `io::Error` saying whether the disk filled or the pipe broke in `source()`.
impl From<webm_iterable::errors::TagWriterError> for Error {
	fn from(err: webm_iterable::errors::TagWriterError) -> Self {
		Error::MatroskaWrite(crate::error::message(err))
	}
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod error_tests {
	use super::*;

	/// `TagWriterError::WriteError` displays only "Error writing to destination.", so the
	/// `io::Error` naming the actual failure has to come through the source chain or an export
	/// failure is undiagnosable.
	#[test]
	fn matroska_write_keeps_the_io_cause() {
		let source = std::io::Error::new(std::io::ErrorKind::StorageFull, "no space left on device");
		let err = Error::from(webm_iterable::errors::TagWriterError::WriteError { source });

		let message = err.to_string();
		assert!(message.contains("no space left on device"), "{message}");
	}
}
