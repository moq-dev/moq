//! MSF catalog.
//!
//! The IETF MoQT Streaming Format catalog, served on the `catalog`
//! track. moq-mux subscribes here but doesn't publish; [`Consumer`]
//! reads MSF and converts it to a [`Catalog<E>`](crate::catalog::hang::Catalog)
//! on the fly so the rest of the pipeline only sees one shape. Publishing is
//! the hang producer's job (it writes both tracks).

mod consumer;

pub use consumer::Consumer;

#[cfg(test)]
pub(crate) use consumer::from_msf;

/// Whether an MSF `namespace` names the catalog's own broadcast at `base`.
///
/// MSF namespaces are absolute, but each side sees paths under its own connection root. Draft
/// catalogs repeat the publisher's full namespace on every track, so a namespace that matches
/// the catalog's path on whole trailing segments is the catalog seen from a different root.
pub(crate) fn names_catalog(namespace: &moq_net::Path, base: &moq_net::Path) -> bool {
	if namespace == base {
		return true;
	}
	let namespace: Vec<_> = namespace.parts().collect();
	let base: Vec<_> = base.parts().collect();
	!namespace.is_empty() && !base.is_empty() && (namespace.ends_with(&base) || base.ends_with(&namespace))
}

/// MSF catalog decoding errors.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The namespace cannot be represented by a relative broadcast reference.
	#[error("MSF track {name:?} namespace {namespace:?} cannot be referenced from the catalog")]
	UnresolvableNamespace { name: String, namespace: String },

	/// Multiple namespaces use a track name that would overwrite one hang rendition ID.
	#[error("MSF track name {0:?} is ambiguous as a hang rendition ID")]
	DuplicateTrackName(String),

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

	/// The track's packaging cannot be converted without discarding it.
	#[error("MSF track {name:?} has unsupported packaging {packaging:?}")]
	UnsupportedPackaging { name: String, packaging: String },

	#[error("MSF text track {0:?} missing codec")]
	MissingTextCodec(String),

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

	#[error("MSF audio track {0:?} has malformed FLAC STREAMINFO")]
	MalformedFlac(String),

	#[error("MSF audio track {0:?} OpusHead has trailing bytes")]
	OpusTrailingBytes(String),

	#[error("MSF audio track {0:?} omits samplerate/channelConfig; codec has no init_data parser")]
	UnsupportedDerivationCodec(String),

	#[error("MSF track {0:?} init segment is malformed")]
	MalformedInitSegment(String),

	#[error("MSF track {0:?} init segment missing moov")]
	MissingInitMoov(String),

	#[error("MSF audio track {0:?} CMAF init has no audio sample entry to derive samplerate/channelConfig from")]
	MissingAudioSampleEntry(String),
}

pub type Result<T> = std::result::Result<T, Error>;
