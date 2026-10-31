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
	/// The algorithm does not match the key type.
	#[error("invalid algorithm for key type")]
	InvalidAlgorithm,

	/// The algorithm does not match the named curve.
	#[error("invalid algorithm for {0} curve")]
	InvalidAlgorithmForCurve(&'static str),

	/// An EC coordinate has the wrong length for the named curve.
	#[error("invalid coordinate length for {0}")]
	InvalidCoordinateLength(&'static str),

	/// The curve does not fit the named key type.
	#[error("invalid curve for {0} key")]
	InvalidCurve(&'static str),

	/// Signing needs a private key, but the key is public.
	#[error("missing private key")]
	MissingPrivateKey,

	/// A symmetric secret is shorter than the minimum length in bytes.
	#[error("oct key secret must be at least {0} bytes")]
	SecretTooShort(usize),

	/// A symmetric key has no public half to export.
	#[error("OCT key cannot be converted to public key")]
	NoPublicKey,

	/// The key's operations exclude verification.
	#[error("key does not support verification")]
	VerifyUnsupported,

	/// The key's operations exclude signing.
	#[error("key does not support signing")]
	SignUnsupported,

	/// The key set has no key that can sign.
	#[error("cannot find signing key")]
	NoSigningKey,

	/// The key set has no key with this `kid`.
	#[error("cannot find key with kid {0}")]
	KeyNotFound(String),

	/// The JWT header has no `kid` to select a key with.
	#[error("missing kid in JWT header")]
	MissingKid,

	/// An EC key has no x coordinate.
	#[error("missing x() point in EC key")]
	MissingEcX,

	/// An EC key has no y coordinate.
	#[error("missing y() point in EC key")]
	MissingEcY,
}

/// Top-level error type for moq-auth.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// A key could not be configured or used.
	#[error(transparent)]
	Key(#[from] KeyError),

	/// The token grants neither publish nor subscribe.
	#[error("no publish or subscribe allowed; token is useless")]
	UselessToken,

	/// A root being minted into a token or key scope is not spelled as a path must be.
	#[error("invalid root {0}")]
	InvalidRoot(String),

	/// The path does not overlap the token's root.
	#[error("path {0:?} does not overlap the token root")]
	RootMismatch(String),

	/// The token grants no access to this path.
	#[error("token grants no access to path {0:?}")]
	NoAccess(String),

	/// The key scope allows neither publish nor subscribe.
	#[error("no publish or subscribe allowed; key scope is useless")]
	UselessScope,

	/// The token claims more than the key's scope allows.
	#[error("token capabilities exceed the key scope")]
	ScopeExceeded,

	/// The algorithm name is not supported.
	#[error("invalid algorithm: {0}")]
	InvalidAlgorithm(String),

	/// The token's expiry has passed.
	#[error("token has expired")]
	TokenExpired,

	/// The token's not-before time has not arrived.
	#[error("token is not valid yet")]
	TokenNotYetValid,

	/// A path pattern did not parse.
	#[error(transparent)]
	Pattern(#[from] moq_pattern::InvalidPattern),

	/// The grant allows nothing, so the session is refused.
	#[error("grant names nothing; the session is refused")]
	UselessGrant,

	/// The grant marks an upstream but not a peer.
	#[error("grant marks an upstream that is not a peer")]
	UpstreamWithoutPeer,

	/// The grant asks to be revalidated but never expires.
	#[error("grant asks to be revalidated but never expires")]
	UnboundedRevalidate,

	/// The grant sets session limits without a revalidate cadence.
	#[error("session limits need a revalidate cadence, which ages out the slots of a relay that died")]
	LimitsWithoutRevalidate,

	/// The grant's expiry is beyond what the system clock can represent.
	#[error("the grant bound reaches past the system clock's range")]
	ExpiresOutOfRange,

	/// The grant's revalidate interval is zero.
	#[error("grant asks to be revalidated at no interval")]
	ZeroRevalidate,

	/// No ES256 or EdDSA key is available to sign or verify a peer grant.
	#[error("peer grants need an ES256 or EdDSA key")]
	NoPeerKey,

	/// A peer grant's presenter key carries private material.
	#[error("a peer grant's presenter key must be public")]
	PrivatePresenter,

	/// The grant's expiry has passed.
	#[error("grant has already expired")]
	GrantExpired,

	/// A stats label is not a literal path.
	#[error("invalid stats label: {0}")]
	InvalidLabel(moq_pattern::InvalidPattern),

	/// A stats tier is not `/`-joined segments of ASCII letters, digits, `.`, `_`, and `-`.
	#[error("invalid stats tier {0:?}: use segments of ASCII letters, digits, '.', '_', and '-' joined by '/'")]
	InvalidTier(String),

	/// The auth server refused the session.
	#[error(transparent)]
	Refused(#[from] crate::Refusal),

	/// The auth server could not be reached or answered with an error.
	#[error("auth server unavailable: {0}")]
	Unavailable(String),

	/// The auth URL is plain http:// to a host other than loopback.
	#[error("auth URL must be https://, unix://, or http:// to a loopback address: {0}")]
	InsecureUrl(String),

	/// The auth URL has an unsupported scheme, or is a `unix:` URL that names no socket path.
	#[error("invalid auth URL: {0}")]
	InvalidUrl(String),

	/// A JWK or claims document couldn't be parsed or serialized.
	#[error("{0}")]
	Json(String),

	/// A local file could not be read or written.
	#[error(transparent)]
	Io(#[from] std::io::Error),

	/// A base64url field (a JWK coordinate, a JWT segment) isn't valid base64.
	#[error("{0}")]
	Base64(String),

	/// A decoded field is not valid UTF-8.
	#[error(transparent)]
	Utf8(#[from] std::string::FromUtf8Error),

	/// The JWT itself couldn't be signed, decoded, or verified.
	#[error("{0}")]
	Jwt(String),

	/// A key couldn't be parsed, imported, or used by the crypto backend.
	#[error("{0}")]
	Crypto(String),

	/// Fetching a remote JWKS failed.
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
	aws_lc_rs::error::Unspecified => Crypto,
	aws_lc_rs::error::KeyRejected => Crypto,
}

#[cfg(feature = "client")]
impl From<reqwest::Error> for Error {
	fn from(err: reqwest::Error) -> Self {
		// reqwest prints the full URL in its error, and a dialed URL can carry
		// credentials in its query or userinfo.
		Self::Unavailable(message(err.without_url()))
	}
}

/// A [`Result`](std::result::Result) with this crate's [`Error`].
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
