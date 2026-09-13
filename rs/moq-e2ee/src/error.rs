/// Typed failures from [draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/).
///
/// Display is the profile code. Secrets, keys, and plaintext never appear here.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// Credential names a profile other than [`crate::PROFILE`].
	#[error("unsupported_profile")]
	UnsupportedProfile,

	/// Secret is not 32 bytes.
	#[error("invalid_secret")]
	InvalidSecret,

	/// An integer is outside the profile bounds, a `bytes` field exceeds 65535, or `domain` is not grouped/datagram.
	#[error("identity")]
	Identity,

	/// The next AEAD operation would exceed `2^24` uses of that key or `2^36` plaintext bytes under that key.
	#[error("exhausted")]
	Exhausted,

	/// Encrypting different bytes at an identity that already produced ciphertext, or restarting publication under the same generation.
	#[error("reuse")]
	Reuse,

	/// Plaintext plus tag exceeds the transport payload limit, or a ciphertext is shorter than the tag or larger than that limit.
	#[error("oversize")]
	Oversize,

	/// AEAD open failed, including relocation across any identity field.
	#[error("authentication")]
	Authentication,

	/// A receiver has already opened this identity inside its retained window.
	#[error("duplicate")]
	Duplicate,

	/// The credential is not the generation or kid the application pinned.
	#[error("pinned_mismatch")]
	PinnedMismatch,

	/// The underlying MoQ track failed.
	#[error(transparent)]
	Net(#[from] moq_net::Error),
}

impl Error {
	/// The profile code for this failure, or `net` for a transport error.
	pub fn code(&self) -> &'static str {
		match self {
			Self::UnsupportedProfile => "unsupported_profile",
			Self::InvalidSecret => "invalid_secret",
			Self::Identity => "identity",
			Self::Exhausted => "exhausted",
			Self::Reuse => "reuse",
			Self::Oversize => "oversize",
			Self::Authentication => "authentication",
			Self::Duplicate => "duplicate",
			Self::PinnedMismatch => "pinned_mismatch",
			Self::Net(_) => "net",
		}
	}
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
