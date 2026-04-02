use crate::Error;
use std::{fmt, str::FromStr};

/// A subset of jsonwebtoken algorithms.
///
/// We could support all of them, but there's currently no point using public key crypto.
/// The relay can fetch any resource it wants; it doesn't need to forge tokens.
///
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq, Hash)]
pub enum Algorithm {
	HS256,
	HS384,
	HS512,
	ES256,
	ES384,
	RS256,
	RS384,
	RS512,
	PS256,
	PS384,
	PS512,
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
	fn test_algorithm_from_str_valid() {
		assert_eq!(Algorithm::from_str("HS256").unwrap(), Algorithm::HS256);
		assert_eq!(Algorithm::from_str("HS384").unwrap(), Algorithm::HS384);
		assert_eq!(Algorithm::from_str("HS512").unwrap(), Algorithm::HS512);
		assert_eq!(Algorithm::from_str("ES256").unwrap(), Algorithm::ES256);
		assert_eq!(Algorithm::from_str("ES384").unwrap(), Algorithm::ES384);
		assert_eq!(Algorithm::from_str("RS256").unwrap(), Algorithm::RS256);
		assert_eq!(Algorithm::from_str("RS384").unwrap(), Algorithm::RS384);
		assert_eq!(Algorithm::from_str("RS512").unwrap(), Algorithm::RS512);
		assert_eq!(Algorithm::from_str("PS256").unwrap(), Algorithm::PS256);
		assert_eq!(Algorithm::from_str("PS384").unwrap(), Algorithm::PS384);
		assert_eq!(Algorithm::from_str("PS512").unwrap(), Algorithm::PS512);
		assert_eq!(Algorithm::from_str("EdDSA").unwrap(), Algorithm::EdDSA);
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
	fn test_algorithm_display() {
		assert_eq!(Algorithm::HS256.to_string(), "HS256");
		assert_eq!(Algorithm::HS384.to_string(), "HS384");
		assert_eq!(Algorithm::HS512.to_string(), "HS512");
		assert_eq!(Algorithm::ES256.to_string(), "ES256");
		assert_eq!(Algorithm::ES384.to_string(), "ES384");
		assert_eq!(Algorithm::RS256.to_string(), "RS256");
		assert_eq!(Algorithm::RS384.to_string(), "RS384");
		assert_eq!(Algorithm::RS512.to_string(), "RS512");
		assert_eq!(Algorithm::PS256.to_string(), "PS256");
		assert_eq!(Algorithm::PS384.to_string(), "PS384");
		assert_eq!(Algorithm::PS512.to_string(), "PS512");
		assert_eq!(Algorithm::EdDSA.to_string(), "EdDSA");
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

	#[test]
	fn test_algorithm_equality() {
		assert_eq!(Algorithm::HS256, Algorithm::HS256);
		assert_ne!(Algorithm::HS256, Algorithm::HS384);
		assert_ne!(Algorithm::HS384, Algorithm::HS512);
		assert_eq!(Algorithm::ES256, Algorithm::ES256);
		assert_eq!(Algorithm::ES384, Algorithm::ES384);
		assert_ne!(Algorithm::HS384, Algorithm::ES256);
		assert_ne!(Algorithm::ES256, Algorithm::ES384);
		assert_eq!(Algorithm::RS256, Algorithm::RS256);
		assert_eq!(Algorithm::RS384, Algorithm::RS384);
		assert_eq!(Algorithm::RS512, Algorithm::RS512);
		assert_ne!(Algorithm::RS256, Algorithm::RS512);
		assert_ne!(Algorithm::RS256, Algorithm::PS256);
		assert_eq!(Algorithm::PS256, Algorithm::PS256);
		assert_eq!(Algorithm::PS384, Algorithm::PS384);
		assert_eq!(Algorithm::PS512, Algorithm::PS512);
		assert_ne!(Algorithm::PS256, Algorithm::PS512);
		assert_eq!(Algorithm::EdDSA, Algorithm::EdDSA);
		assert_ne!(Algorithm::EdDSA, Algorithm::ES256);
		assert_ne!(Algorithm::EdDSA, Algorithm::RS512);
	}

	#[test]
	fn test_algorithm_clone() {
		let alg = Algorithm::HS256;
		let cloned = alg;
		assert_eq!(alg, cloned);
	}
}
