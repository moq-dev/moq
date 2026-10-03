use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One publisher instance's identity, the lowercase hyphenated text of a UUIDv7.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(Arc<str>);

/// The epoch is not a lowercase hyphenated UUIDv7 with the RFC variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid epoch: expected a lowercase hyphenated UUIDv7")]
#[non_exhaustive]
pub struct InvalidEpoch;

impl Epoch {
	/// Mint a fresh identity using the wall clock and secure randomness, ordered newest last.
	pub fn mint() -> Self {
		Self(uuid::Uuid::now_v7().hyphenated().to_string().into())
	}

	/// The UUID text, without the path segment's `@` marker.
	pub fn as_str(&self) -> &str {
		&self.0
	}

	/// The wall-clock time encoded in the UUID, with millisecond precision.
	pub fn time(&self) -> SystemTime {
		// Parsing enforces the canonical UUID form, whose first 48 bits are Unix milliseconds.
		let millis =
			u64::from_str_radix(&self.0[..8], 16).unwrap() << 16 | u64::from_str_radix(&self.0[9..13], 16).unwrap();
		UNIX_EPOCH + Duration::from_millis(millis)
	}
}

impl FromStr for Epoch {
	type Err = InvalidEpoch;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		let uuid = uuid::Uuid::parse_str(text).map_err(|_| InvalidEpoch)?;
		if uuid.get_version_num() != 7
			|| uuid.get_variant() != uuid::Variant::RFC4122
			|| uuid.hyphenated().to_string() != text
		{
			return Err(InvalidEpoch);
		}
		Ok(Self(text.into()))
	}
}

impl fmt::Debug for Epoch {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("Epoch").field(&self.as_str()).finish()
	}
}

impl fmt::Display for Epoch {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl AsRef<str> for Epoch {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn mint_is_canonical_and_ordered() {
		let epoch = Epoch::mint();
		assert_eq!(epoch.as_str().parse::<Epoch>().unwrap(), epoch);
		assert!(Epoch::mint() > epoch);
	}
}
