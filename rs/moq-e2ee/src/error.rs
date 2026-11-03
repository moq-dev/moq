/// Typed failures from [draft-lcurley-moq-e2ee](https://datatracker.ietf.org/doc/draft-lcurley-moq-e2ee/).
///
/// Display is the profile code. Secrets, keys, and plaintext never appear here.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum Error {
	/// An integer is outside the profile bounds, a `bytes` field exceeds 65535, or a physical name does not parse.
	#[error("identity")]
	Identity,

	/// The next AEAD operation would exceed `2^24` uses of that key or `2^36` plaintext bytes under that key.
	#[error("exhausted")]
	Exhausted,

	/// Encrypting at a sequence this generation already allocated for the track's name.
	#[error("reuse")]
	Reuse,

	/// Plaintext plus tag exceeds the transport payload limit, or a ciphertext is shorter than the tag or larger than that limit.
	#[error("oversize")]
	Oversize,

	/// AEAD open failed, including relocation across any identity field, or a physical name does not open under its generation.
	#[error("authentication")]
	Authentication,

	/// This CPU lacks an instruction set extension the crypto requires; the message names it.
	#[error("unsupported: {0}")]
	Unsupported(String),

	/// The underlying MoQ track failed.
	#[error(transparent)]
	Net(#[from] moq_net::Error),
}

/// A [`Result`](std::result::Result) using this crate's [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
