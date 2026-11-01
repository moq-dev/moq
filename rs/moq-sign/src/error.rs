/// Typed failures from [draft-lcurley-moq-sign](https://datatracker.ietf.org/doc/draft-lcurley-moq-sign/).
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// A frame is shorter than a signature, or its signature does not verify at its identity.
	#[error("signature")]
	Signature,

	/// The broadcast was not resolved through a route that names its publisher epoch.
	#[error("epoch")]
	Epoch,

	/// The underlying MoQ broadcast, track, or group failed.
	#[error(transparent)]
	Net(#[from] moq_net::Error),
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
