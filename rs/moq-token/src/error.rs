/// Renders an error and its `source()` chain into a single message.
///
/// Dependency errors are stored as messages so their crates stay out of this crate's public
/// API. Several of them keep the actionable half in `source()` and nothing but a category in
/// `Display`, so a plain `to_string()` would drop the only detail worth reporting.
pub(crate) fn message(err: impl std::error::Error) -> String {
	use std::fmt::Write;

	let mut out = err.to_string();
	let mut source = err.source();
	while let Some(err) = source {
		let _ = write!(out, ": {err}");
		source = err.source();
	}
	out
}

/// Errors related to key configuration and cryptographic operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
	#[error("invalid algorithm for key type")]
	InvalidAlgorithm,

	#[error("invalid algorithm for {0} curve")]
	InvalidAlgorithmForCurve(&'static str),

	#[error("invalid coordinate length for {0}")]
	InvalidCoordinateLength(&'static str),

	#[error("invalid curve for {0} key")]
	InvalidCurve(&'static str),

	#[error("missing private key")]
	MissingPrivateKey,

	#[error("oct key secret must be at least {0} bytes")]
	SecretTooShort(usize),

	#[error("OCT key cannot be converted to public key")]
	NoPublicKey,

	#[error("key does not support verification")]
	VerifyUnsupported,

	#[error("key does not support signing")]
	SignUnsupported,

	#[error("cannot find signing key")]
	NoSigningKey,

	#[error("cannot find key with kid {0}")]
	KeyNotFound(String),

	#[error("missing kid in JWT header")]
	MissingKid,

	#[error("missing x() point in EC key")]
	MissingEcX,

	#[error("missing y() point in EC key")]
	MissingEcY,
}

/// Top-level error type for moq-token.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	#[error(transparent)]
	Key(#[from] KeyError),

	#[error("no publish or subscribe allowed; token is useless")]
	UselessToken,

	#[error("path `{0}` does not overlap the token root")]
	RootMismatch(String),

	#[error("token grants no access to path `{0}`")]
	NoAccess(String),

	#[error("no publish or subscribe allowed; key scope is useless")]
	UselessScope,

	#[error("token capabilities exceed the key scope")]
	ScopeExceeded,

	#[error("invalid algorithm: {0}")]
	InvalidAlgorithm(String),

	#[error("token has expired")]
	TokenExpired,

	/// A JWK or claims document couldn't be parsed or serialized.
	#[error("{0}")]
	Json(String),

	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// A base64url field (a JWK coordinate, a JWT segment) isn't valid base64.
	#[error("{0}")]
	Base64(String),

	#[error(transparent)]
	Utf8(#[from] std::string::FromUtf8Error),

	/// The JWT itself couldn't be signed, decoded, or verified.
	#[error("{0}")]
	Jwt(String),

	/// A key couldn't be parsed, imported, or used by the crypto backend.
	#[error("{0}")]
	Crypto(String),

	/// Fetching a remote JWKS failed.
	#[cfg(feature = "jwks-loader")]
	#[error("{0}")]
	Fetch(String),

	#[error("{0}")]
	Other(String),
}

// Dependency errors are flattened to their message so their crates stay out of this crate's
// public API. Every one of them is opaque to a caller anyway: there is nothing to match on,
// only something to report.
macro_rules! from_message {
	($($ty:ty => $variant:ident),* $(,)?) => {
		$(
			impl From<$ty> for Error {
				fn from(err: $ty) -> Self {
					Self::$variant(message(err))
				}
			}
		)*
	};
}

from_message! {
	serde_json::Error => Json,
	base64::DecodeError => Base64,
	jsonwebtoken::errors::Error => Jwt,
	p256::elliptic_curve::pkcs8::Error => Crypto,
	p256::elliptic_curve::Error => Crypto,
	rsa::Error => Crypto,
	rsa::pkcs1::Error => Crypto,
	aws_lc_rs::error::Unspecified => Crypto,
	aws_lc_rs::error::KeyRejected => Crypto,
}

#[cfg(feature = "jwks-loader")]
from_message! {
	reqwest::Error => Fetch,
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
	use super::*;

	/// A dependency that reports only a category in `Display` and keeps the real cause in
	/// `source()` (reqwest is the one that matters here) must not lose it on conversion.
	#[test]
	fn message_flattens_the_source_chain() {
		#[derive(Debug, thiserror::Error)]
		#[error("inner")]
		struct Inner;

		#[derive(Debug, thiserror::Error)]
		#[error("outer")]
		struct Outer(#[source] Inner);

		assert_eq!(message(Outer(Inner)), "outer: inner");
	}
}
