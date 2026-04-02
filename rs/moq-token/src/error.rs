/// Errors related to key configuration and cryptographic operations.
#[derive(Debug, thiserror::Error)]
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
pub enum Error {
	#[error(transparent)]
	Key(#[from] KeyError),

	#[error("no publish or subscribe allowed; token is useless")]
	UselessToken,

	#[error("invalid algorithm: {0}")]
	InvalidAlgorithm(String),

	#[error("token has expired")]
	TokenExpired,

	#[error(transparent)]
	Json(#[from] serde_json::Error),

	#[error(transparent)]
	Io(#[from] std::io::Error),

	#[error(transparent)]
	Base64(#[from] base64::DecodeError),

	#[error(transparent)]
	Utf8(#[from] std::string::FromUtf8Error),

	#[error(transparent)]
	Jwt(#[from] jsonwebtoken::errors::Error),

	#[error(transparent)]
	Pkcs8(#[from] elliptic_curve::pkcs8::Error),

	#[error(transparent)]
	EllipticCurve(#[from] elliptic_curve::Error),

	#[error(transparent)]
	Rsa(#[from] rsa::Error),

	#[error(transparent)]
	RsaPkcs1(#[from] rsa::pkcs1::Error),

	#[error(transparent)]
	AwsUnspecified(#[from] aws_lc_rs::error::Unspecified),

	#[error(transparent)]
	AwsKeyRejected(#[from] aws_lc_rs::error::KeyRejected),

	#[cfg(feature = "jwks-loader")]
	#[error(transparent)]
	Reqwest(#[from] reqwest::Error),

	#[error("{0}")]
	Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
