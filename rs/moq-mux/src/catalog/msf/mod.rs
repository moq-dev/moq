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
	UnresolvableNamespace {
		/// The track name.
		name: String,
		/// The namespace that cannot be resolved.
		namespace: String,
	},

	/// The catalog frame is not valid UTF-8.
	#[error("MSF catalog frame is not valid UTF-8")]
	InvalidUtf8,

	/// The catalog frame cannot be decoded as an MSF catalog.
	#[error("failed to parse MSF catalog frame")]
	ParseFrame,

	/// The named CMAF track has no initialization data.
	#[error("MSF CMAF track {0:?} missing init_data")]
	MissingCmafInit(String),

	/// The named track has malformed initialization data.
	#[error("MSF track {0:?} has malformed init_data")]
	MalformedInitData(String),

	/// The named video track has no codec identifier.
	#[error("MSF video track {0:?} missing codec")]
	MissingVideoCodec(String),

	/// The named audio track has no codec identifier.
	#[error("MSF audio track {0:?} missing codec")]
	MissingAudioCodec(String),

	/// The track's packaging cannot be converted without discarding it.
	#[error("MSF track {name:?} has unsupported packaging {packaging:?}")]
	UnsupportedPackaging {
		/// The track name.
		name: String,
		/// The unsupported packaging identifier.
		packaging: String,
	},

	/// The named text track has no codec identifier.
	#[error("MSF text track {0:?} missing codec")]
	MissingTextCodec(String),

	/// The named video track has an invalid codec identifier.
	#[error("MSF video track {name:?} has invalid codec {codec:?}")]
	InvalidVideoCodec {
		/// The video track name.
		name: String,
		/// The invalid codec identifier.
		codec: String,
	},

	/// The named audio track has an invalid codec identifier.
	#[error("MSF audio track {name:?} has invalid codec {codec:?}")]
	InvalidAudioCodec {
		/// The audio track name.
		name: String,
		/// The invalid codec identifier.
		codec: String,
	},

	/// The named audio track has neither explicit audio parameters nor initialization data.
	#[error("MSF audio track {0:?} omits samplerate/channelConfig and has no init_data to derive from")]
	MissingAudioParams(String),

	/// The track packaging cannot supply the missing audio parameters.
	#[error("MSF audio track {name:?} packaging {packaging:?} is unsupported for parameter derivation")]
	UnsupportedDerivationPackaging {
		/// The audio track name.
		name: String,
		/// The packaging identifier that cannot supply audio parameters.
		packaging: String,
	},

	/// The named audio track has a malformed AudioSpecificConfig.
	#[error("MSF audio track {0:?} has malformed AudioSpecificConfig")]
	MalformedAac(String),

	/// The named audio track has a malformed OpusHead.
	#[error("MSF audio track {0:?} has malformed OpusHead")]
	MalformedOpus(String),

	/// The named audio track has malformed FLAC STREAMINFO.
	#[error("MSF audio track {0:?} has malformed FLAC STREAMINFO")]
	MalformedFlac(String),

	/// The named audio track has bytes after its OpusHead.
	#[error("MSF audio track {0:?} OpusHead has trailing bytes")]
	OpusTrailingBytes(String),

	/// The named audio track uses a codec with no initialization data parser.
	#[error("MSF audio track {0:?} omits samplerate/channelConfig; codec has no init_data parser")]
	UnsupportedDerivationCodec(String),

	/// The named track has a malformed initialization segment.
	#[error("MSF track {0:?} init segment is malformed")]
	MalformedInitSegment(String),

	/// The named track has no moov box in its initialization segment.
	#[error("MSF track {0:?} init segment missing moov")]
	MissingInitMoov(String),

	/// The named audio track has no audio sample entry to supply its audio parameters.
	#[error("MSF audio track {0:?} CMAF init has no audio sample entry to derive samplerate/channelConfig from")]
	MissingAudioSampleEntry(String),
}

/// A result using this module's error type.
pub type Result<T> = std::result::Result<T, Error>;
