/// Errors from moq-mux operations.
///
/// Most variants are simple delegations to underlying layers — [`moq_net::Error`] for
/// transport / pub-sub failures, [`hang::Error`] for catalog/codec parsing, and
/// [`fmp4::Error`](crate::container::fmp4::Error) for CMAF wire-format problems.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// Error from the underlying moq-net transport.
	#[error("moq: {0}")]
	Moq(#[from] moq_net::Error),

	/// Error from the hang catalog/codec layer.
	#[error("hang: {0}")]
	Hang(#[from] hang::Error),

	/// Error publishing or consuming JSON over a track.
	#[error("json: {0}")]
	Json(#[from] moq_json::Error),

	/// Error parsing or building CMAF moof+mdat fragments.
	#[error("cmaf: {0}")]
	Cmaf(#[from] crate::container::fmp4::Error),

	/// Error parsing or building LOC frames.
	#[error("loc: {0}")]
	Loc(#[from] moq_loc::Error),

	/// A frame arrived with no open group to anchor it, i.e. before the first
	/// keyframe. A MoQ group must start with a keyframe, so importers reject such
	/// frames. A stream that can legitimately join mid-GOP (TS) ignores this via
	/// [`is_missing_keyframe`](Self::is_missing_keyframe); formats that must open on
	/// a keyframe (FLV) let it propagate as a malformed-stream error.
	#[error("frame received before the first keyframe")]
	MissingKeyframe,
}

impl Error {
	/// Whether `err` (possibly wrapped by anyhow) is [`Error::MissingKeyframe`].
	///
	/// Container importers that join mid-stream use this to drop the leading deltas
	/// a codec importer rejects before a keyframe anchors the first group.
	pub fn is_missing_keyframe(err: &anyhow::Error) -> bool {
		err.downcast_ref::<Error>()
			.is_some_and(|e| matches!(e, Error::MissingKeyframe))
	}
}

/// A Result type alias for moq-mux operations.
pub type Result<T> = std::result::Result<T, Error>;
