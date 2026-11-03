use crate::Error;
use std::{fmt, str::FromStr};

/// A subset of jsonwebtoken algorithms.
///
/// We could support all of them, but there's currently no point using public key crypto.
/// The relay can fetch any resource it wants; it doesn't need to forge tokens.
///
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash)]
pub enum Algorithm {
	/// HMAC with SHA-256.
	HS256,
	/// HMAC with SHA-384.
	HS384,
	/// HMAC with SHA-512.
	HS512,
	/// ECDSA on P-256 with SHA-256.
	ES256,
	/// ECDSA on P-384 with SHA-384.
	ES384,
	/// RSASSA-PKCS1-v1_5 with SHA-256.
	RS256,
	/// RSASSA-PKCS1-v1_5 with SHA-384.
	RS384,
	/// RSASSA-PKCS1-v1_5 with SHA-512.
	RS512,
	/// RSASSA-PSS with SHA-256.
	PS256,
	/// RSASSA-PSS with SHA-384.
	PS384,
	/// RSASSA-PSS with SHA-512.
	PS512,
	/// EdDSA on Ed25519.
	EdDSA,
}

impl From<Algorithm> for jsonwebtoken::Algorithm {
	fn from(val: Algorithm) -> Self {
		match val {
			Algorithm::HS256 => jsonwebtoken::Algorithm::HS256,
			Algorithm::HS384 => jsonwebtoken::Algorithm::HS384,
			Algorithm::HS512 => jsonwebtoken::Algorithm::HS512,
			Algorithm::ES256 => jsonwebtoken::Algorithm::ES256,
			Algorithm::ES384 => jsonwebtoken::Algorithm::ES384,
			Algorithm::RS256 => jsonwebtoken::Algorithm::RS256,
			Algorithm::RS384 => jsonwebtoken::Algorithm::RS384,
			Algorithm::RS512 => jsonwebtoken::Algorithm::RS512,
			Algorithm::PS256 => jsonwebtoken::Algorithm::PS256,
			Algorithm::PS384 => jsonwebtoken::Algorithm::PS384,
			Algorithm::PS512 => jsonwebtoken::Algorithm::PS512,
			Algorithm::EdDSA => jsonwebtoken::Algorithm::EdDSA,
		}
	}
}

impl FromStr for Algorithm {
	type Err = Error;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"HS256" => Ok(Algorithm::HS256),
			"HS384" => Ok(Algorithm::HS384),
			"HS512" => Ok(Algorithm::HS512),
			"ES256" => Ok(Algorithm::ES256),
			"ES384" => Ok(Algorithm::ES384),
			"RS256" => Ok(Algorithm::RS256),
			"RS384" => Ok(Algorithm::RS384),
			"RS512" => Ok(Algorithm::RS512),
			"PS256" => Ok(Algorithm::PS256),
			"PS384" => Ok(Algorithm::PS384),
			"PS512" => Ok(Algorithm::PS512),
			"EdDSA" => Ok(Algorithm::EdDSA),
			_ => Err(Error::InvalidAlgorithm(s.to_string())),
		}
	}
}

impl fmt::Display for Algorithm {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Algorithm::HS256 => write!(f, "HS256"),
			Algorithm::HS384 => write!(f, "HS384"),
			Algorithm::HS512 => write!(f, "HS512"),
			Algorithm::ES256 => write!(f, "ES256"),
			Algorithm::ES384 => write!(f, "ES384"),
			Algorithm::RS256 => write!(f, "RS256"),
			Algorithm::RS384 => write!(f, "RS384"),
			Algorithm::RS512 => write!(f, "RS512"),
			Algorithm::PS256 => write!(f, "PS256"),
			Algorithm::PS384 => write!(f, "PS384"),
			Algorithm::PS512 => write!(f, "PS512"),
			Algorithm::EdDSA => write!(f, "EdDSA"),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_algorithm_from_str_and_display() {
		for (name, alg) in [
			("HS256", Algorithm::HS256),
			("HS384", Algorithm::HS384),
			("HS512", Algorithm::HS512),
			("ES256", Algorithm::ES256),
			("ES384", Algorithm::ES384),
			("RS256", Algorithm::RS256),
			("RS384", Algorithm::RS384),
			("RS512", Algorithm::RS512),
			("PS256", Algorithm::PS256),
			("PS384", Algorithm::PS384),
			("PS512", Algorithm::PS512),
			("EdDSA", Algorithm::EdDSA),
		] {
			assert_eq!(Algorithm::from_str(name).unwrap(), alg);
			assert_eq!(alg.to_string(), name);
		}
	}

	#[test]
	fn test_algorithm_from_str_invalid() {
		assert!(Algorithm::from_str("HS128").is_err());
		assert!(Algorithm::from_str("RS128").is_err());
		assert!(Algorithm::from_str("ES512").is_err());
		assert!(Algorithm::from_str("EDDSA").is_err());
		assert!(Algorithm::from_str("invalid").is_err());
		assert!(Algorithm::from_str("").is_err());
	}

	#[test]
	fn test_algorithm_to_jsonwebtoken_algorithm() {
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::HS256),
			jsonwebtoken::Algorithm::HS256
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::HS384),
			jsonwebtoken::Algorithm::HS384
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::HS512),
			jsonwebtoken::Algorithm::HS512
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::ES256),
			jsonwebtoken::Algorithm::ES256
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::ES384),
			jsonwebtoken::Algorithm::ES384
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::RS256),
			jsonwebtoken::Algorithm::RS256
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::RS384),
			jsonwebtoken::Algorithm::RS384
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::RS512),
			jsonwebtoken::Algorithm::RS512
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::PS256),
			jsonwebtoken::Algorithm::PS256
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::PS384),
			jsonwebtoken::Algorithm::PS384
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::PS512),
			jsonwebtoken::Algorithm::PS512
		);
		assert_eq!(
			jsonwebtoken::Algorithm::from(Algorithm::EdDSA),
			jsonwebtoken::Algorithm::EdDSA
		);
	}

	#[test]
	fn test_algorithm_serde() {
		let alg = Algorithm::HS256;
		let json = serde_json::to_string(&alg).unwrap();
		assert_eq!(json, "\"HS256\"");

		let deserialized: Algorithm = serde_json::from_str(&json).unwrap();
		assert_eq!(deserialized, alg);
	}
}
