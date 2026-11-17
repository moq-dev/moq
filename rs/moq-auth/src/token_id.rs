use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Serialize};

/// A token's identifier (its `jti` claim), such as a UUID an issuer records to revoke it.
///
/// One to 128 printable ASCII characters, so it logs safely and both libraries bound it
/// by the same length. An issuer picks it; nothing here invents one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TokenId(String);

impl TokenId {
	const MAX_LENGTH: usize = 128;

	/// Validate and create a token ID from a string.
	pub fn decode(s: &str) -> Result<Self, TokenIdError> {
		if s.is_empty() {
			return Err(TokenIdError::Empty);
		}
		if s.len() > Self::MAX_LENGTH {
			return Err(TokenIdError::TooLong);
		}
		if !s.bytes().all(|b| b.is_ascii_graphic()) {
			return Err(TokenIdError::Invalid);
		}
		Ok(Self(s.to_owned()))
	}

	/// Returns the token ID as a string slice.
	pub fn encode(&self) -> &str {
		&self.0
	}
}

impl fmt::Display for TokenId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&self.0)
	}
}

impl AsRef<str> for TokenId {
	fn as_ref(&self) -> &str {
		&self.0
	}
}

impl Deref for TokenId {
	type Target = str;

	fn deref(&self) -> &str {
		&self.0
	}
}

impl From<TokenId> for String {
	fn from(id: TokenId) -> Self {
		id.0
	}
}

impl TryFrom<String> for TokenId {
	type Error = TokenIdError;

	fn try_from(s: String) -> Result<Self, Self::Error> {
		TokenId::decode(&s)
	}
}

/// Why a string is not a valid token ID.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum TokenIdError {
	/// The token ID is empty.
	#[error("token ID must not be empty")]
	Empty,

	/// The token ID is longer than 128 characters.
	#[error("token ID exceeds maximum length of 128 characters")]
	TooLong,

	/// The token ID has a character outside printable ASCII, such as a space.
	#[error("token ID contains invalid characters (only printable ASCII without spaces allowed)")]
	Invalid,
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn bounds() {
		for valid in [
			"a",
			"550e8400-e29b-41d4-a716-446655440000",
			"user:42/a.b+c=",
			&"x".repeat(128),
		] {
			assert!(TokenId::decode(valid).is_ok(), "{valid}");
		}
		for invalid in ["", "has space", "tab\t", "caf\u{e9}", &"x".repeat(129)] {
			assert!(TokenId::decode(invalid).is_err(), "{invalid}");
		}
	}
}
