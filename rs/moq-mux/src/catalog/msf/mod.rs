//! MSF catalog.
//!
//! The IETF MoQT Streaming Format catalog, served on the `catalog`
//! track. moq-mux subscribes here but doesn't publish; [`Consumer`]
//! reads MSF and converts it to a [`hang::Catalog`] on the fly so the
//! rest of the pipeline only sees one shape. Publishing is the hang
//! producer's job (it writes both tracks).

mod consumer;

pub use consumer::Consumer;

/// MSF catalog decoding errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error("MSF catalog frame is not valid UTF-8")]
	InvalidUtf8,

	#[error("failed to parse MSF catalog frame")]
	ParseFrame,

	#[error("MSF CMAF track {0:?} missing init_data")]
	MissingCmafInit(String),

	#[error("MSF track {0:?} has malformed init_data")]
	MalformedInitData(String),

	#[error("MSF video track {0:?} missing codec")]
	MissingVideoCodec(String),

	#[error("MSF audio track {0:?} missing codec")]
	MissingAudioCodec(String),

	#[error("MSF video track {name:?} has invalid codec {codec:?}")]
	InvalidVideoCodec { name: String, codec: String },

	#[error("MSF audio track {name:?} has invalid codec {codec:?}")]
	InvalidAudioCodec { name: String, codec: String },

	#[error("MSF audio track {0:?} omits samplerate/channelConfig and has no init_data to derive from")]
	MissingAudioParams(String),

	#[error("MSF audio track {name:?} packaging {packaging:?} is unsupported for parameter derivation")]
	UnsupportedDerivationPackaging { name: String, packaging: String },

	#[error("MSF audio track {0:?} has malformed AudioSpecificConfig")]
	MalformedAac(String),

	#[error("MSF audio track {0:?} has malformed OpusHead")]
	MalformedOpus(String),

	#[error("MSF audio track {0:?} OpusHead has trailing bytes")]
	OpusTrailingBytes(String),

	#[error("MSF audio track {0:?} omits samplerate/channelConfig; codec has no init_data parser")]
	UnsupportedDerivationCodec(String),

	#[error("MSF audio track {0:?} init segment is malformed")]
	MalformedInitSegment(String),

	#[error("MSF audio track {0:?} init segment missing moov")]
	MissingInitMoov(String),

	#[error("MSF audio track {0:?} CMAF init has no audio sample entry to derive samplerate/channelConfig from")]
	MissingAudioSampleEntry(String),
}

pub type Result<T> = std::result::Result<T, Error>;
