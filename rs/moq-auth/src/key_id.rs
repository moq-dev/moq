use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

/// A validated key identifier (kid) that is safe for use in file paths and URLs.
///
/// Only allows ASCII alphanumeric characters, hyphens, and underscores.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct KeyId(String);

impl KeyId {
	const MAX_LENGTH: usize = 128;

	/// Validate and create a KeyId from a string.
	pub fn decode(s: &str) -> Result<Self, KeyIdError> {
		if s.is_empty() {
			return Err(KeyIdError::Empty);
		}
		if s.len() > Self::MAX_LENGTH {
			return Err(KeyIdError::TooLong);
		}
		if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
			return Err(KeyIdError::Invalid);
		}
		Ok(Self(s.to_owned()))
	}

	/// Returns the key ID as a string slice.
	pub fn encode(&self) -> &str {
		&self.0
	}

	/// Generate a random key ID using cryptographically secure randomness.
	pub fn random() -> Self {
		let mut bytes = [0u8; 8];
		aws_lc_rs::rand::fill(&mut bytes).expect("failed to generate random bytes");
		let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
		Self(hex)
	}
}

impl fmt::Display for KeyId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.0)
	}
}

impl AsRef<str> for KeyId {
	fn as_ref(&self) -> &str {
		&self.0
	}
}

impl Deref for KeyId {
	type Target = str;

	fn deref(&self) -> &str {
		&self.0
	}
}

impl From<KeyId> for String {
	fn from(kid: KeyId) -> Self {
		kid.0
	}
}

impl TryFrom<String> for KeyId {
	type Error = KeyIdError;

	fn try_from(s: String) -> Result<Self, Self::Error> {
		KeyId::decode(&s)
	}
}

#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum KeyIdError {
	#[error("key ID must not be empty")]
	Empty,

	#[error("key ID exceeds maximum length of 128 characters")]
	TooLong,

	#[error("key ID contains invalid characters (only alphanumeric, hyphens, underscores allowed)")]
	Invalid,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn valid_key_ids() {
		assert!(KeyId::decode("abc-123_DEF").is_ok());
		assert!(KeyId::decode("simple").is_ok());
		assert!(KeyId::decode("key-1").is_ok());
		assert!(KeyId::decode("a").is_ok());
	}

	#[test]
	fn invalid_key_ids() {
		assert!(KeyId::decode("").is_err());
		assert!(KeyId::decode("../etc/passwd").is_err());
		assert!(KeyId::decode("key with spaces").is_err());
		assert!(KeyId::decode("key/slash").is_err());
		assert!(KeyId::decode("key.dot").is_err());
	}

	#[test]
	fn random_key_id_is_valid() {
		let kid = KeyId::random();
		assert!(KeyId::decode(kid.encode()).is_ok());
		assert!(!kid.encode().is_empty());
	}
}
