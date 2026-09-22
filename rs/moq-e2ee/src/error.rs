/// Typed failures from [draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/).
///
/// Display is the profile code. Secrets, keys, and plaintext never appear here.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An integer is outside the profile bounds, a `bytes` field exceeds 65535, an epoch is empty or contains `/`, or a name is not 22 base64url characters.
	#[error("identity")]
	Identity,

	/// The next AEAD operation would exceed `2^24` uses of that key or `2^36` plaintext bytes under that key.
	#[error("exhausted")]
	Exhausted,

	/// Encrypting at a sequence this generation already allocated, or producing a track it already claimed.
	#[error("reuse")]
	Reuse,

	/// Plaintext plus tag exceeds the transport payload limit, or a ciphertext is shorter than the tag or larger than that limit.
	#[error("oversize")]
	Oversize,

	/// AEAD open failed, including relocation across any identity field.
	#[error("authentication")]
	Authentication,

	/// The underlying MoQ track failed.
	#[error(transparent)]
	Net(#[from] moq_net::Error),
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
