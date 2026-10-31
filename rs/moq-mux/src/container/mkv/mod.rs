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
	/// A track field is outside the catalog's numeric range.
	#[error("MKV {0} is outside the catalog's numeric range")]
	ValueOutOfRange(&'static str),

	/// Container display metadata has no valid catalog aspect ratio.
	#[error("invalid display aspect: {0}")]
	InvalidAspect(&'static str),

	/// A subtitle cue cannot be represented with valid text and timing.
	#[error("invalid subtitle: {0}")]
	InvalidSubtitle(&'static str),

	/// The EBML header declares an unsupported DocType.
	#[error("unsupported EBML DocType: {0}")]
	UnsupportedDocType(String),

	/// The EBML header has no DocType.
	#[error("EBML header missing DocType")]
	MissingDocType,

	/// The SimpleBlock payload is malformed.
	#[error("invalid SimpleBlock")]
	InvalidSimpleBlock,

	/// The Block payload is malformed.
	#[error("invalid Block payload")]
	InvalidBlock,

	/// The block timestamp precedes the start of the timeline.
	#[error("negative block timestamp")]
	NegativeBlockTimestamp,

	/// A timestamp calculation overflowed.
	#[error("timestamp overflow")]
	TimestampOverflow,

	/// Input arrived after `finish()`, so any tracks it declares could never finish.
	#[error("Tracks after finish")]
	TracksAfterFinish,

	/// The TrackEntry has no TrackNumber.
	#[error("TrackEntry missing TrackNumber")]
	MissingTrackNumber,

	/// The TrackEntry has no TrackType.
	#[error("TrackEntry missing TrackType")]
	MissingTrackType,

	/// The TrackEntry has no CodecID.
	#[error("TrackEntry missing CodecID")]
	MissingCodecId,

	/// The video track declares an unsupported CodecID.
	#[error("unsupported video CodecID: {0}")]
	UnsupportedVideoCodec(String),

	/// The audio track declares an unsupported CodecID.
	#[error("unsupported audio CodecID: {0}")]
	UnsupportedAudioCodec(String),

	/// The codec lacks the private data required for the stated purpose.
	#[error("{codec_id} missing CodecPrivate ({purpose})")]
	MissingCodecPrivate {
		/// The codec requiring private data.
		codec_id: &'static str,
		/// The operation requiring that private data.
		purpose: &'static str,
	},

	/// The HEVCDecoderConfigurationRecord is malformed.
	#[error("invalid HEVCDecoderConfigurationRecord")]
	InvalidHvcc,

	/// The AV1CodecConfigurationRecord is malformed.
	#[error("invalid AV1CodecConfigurationRecord")]
	InvalidAv1c,

	/// A `Colour` element value has no catalog representation.
	#[error("invalid Colour {0}")]
	InvalidColour(&'static str),

	/// A `StereoMode` or `Projection` element the catalog cannot describe, or a malformed one.
	#[error("unsupported spherical video: {0}")]
	UnsupportedSpatial(&'static str),

	/// The named track joined after the MKV header was emitted.
	#[error("MKV track layout changed after header was emitted: track '{0}' added")]
	HeaderAddedTrack(String),

	/// The named rendition moved to a track the header's TrackEntry does not describe.
	#[error("MKV track '{0}' moved to a track whose codec configuration differs from the header's")]
	TrackChanged(String),

	/// The named CMAF track cannot be exported as MKV.
	#[error("MKV export does not support CMAF {kind} track '{name}'")]
	UnsupportedCmafTrack {
		/// The track media kind.
		kind: String,
		/// The track name.
		name: String,
	},

	/// The named video codec cannot be exported as MKV.
	#[error("MKV export does not support video codec {0}")]
	UnsupportedVideoExport(String),

	/// The named audio codec cannot be exported as MKV.
	#[error("MKV export does not support audio codec {0}")]
	UnsupportedAudioExport(String),

	/// The AAC track has no AudioSpecificConfig description.
	#[error("AAC track missing AudioSpecificConfig (description)")]
	MissingAacDescription,

	/// The FLAC track has no STREAMINFO description.
	#[error("FLAC track missing STREAMINFO (description)")]
	MissingFlacDescription,

	/// The H.264 track has no AVCDecoderConfigurationRecord.
	#[error("H.264 track missing AVCDecoderConfigurationRecord")]
	MissingH264Avcc,

	/// The H.265 track has no HEVCDecoderConfigurationRecord.
	#[error("H.265 track missing HEVCDecoderConfigurationRecord")]
	MissingH265Hvcc,

	/// The block timestamp offset from its cluster overflows.
	#[error("cluster underflow")]
	ClusterUnderflow,

	/// The block timestamp does not fit the signed 16-bit relative field.
	#[error("block timestamp doesn't fit in i16")]
	BlockTimestampOverflow,

	/// The frame references an unavailable track.
	#[error("missing track")]
	MissingTrack,

	/// The timestamp does not fit an unsigned 64-bit millisecond count.
	#[error("timestamp doesn't fit in u64 ms")]
	TimestampU64,

	/// The named video track is absent from the tracks map.
	#[error("video track {0} missing in tracks map")]
	MissingVideoTrack(String),

	/// The named audio track is absent from the tracks map.
	#[error("audio track {0} missing in tracks map")]
	MissingAudioTrack(String),

	/// No catalog snapshot is available to declare the tracks.
	#[error("no catalog snapshot")]
	NoCatalogSnapshot,

	/// The Matroska stream could not be parsed.
	#[error("matroska parse error")]
	MatroskaParse,

	/// The Matroska stream could not be written.
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

/// A result using this module's error type.
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
