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
}

/// A Result type alias for moq-mux operations.
pub type Result<T> = std::result::Result<T, Error>;
