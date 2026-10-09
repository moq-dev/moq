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

	/// The UUID text.
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

impl Epoch {
	/// The UUID's 16 bytes, as the wire carries them.
	pub(crate) fn to_bytes(&self) -> [u8; 16] {
		// Parsing enforces the canonical form, so this cannot fail.
		uuid::Uuid::parse_str(&self.0).unwrap().into_bytes()
	}

	/// Parse the wire's 16 bytes, refusing anything but a UUIDv7 with the RFC variant.
	pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, InvalidEpoch> {
		let uuid = uuid::Uuid::from_slice(bytes).map_err(|_| InvalidEpoch)?;
		uuid.hyphenated().to_string().parse()
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

	/// Shared with `js/net/src/epoch.test.ts`, so both parse and order alike.
	#[test]
	fn epoch_vectors() {
		let vectors: serde_json::Value = serde_json::from_str(include_str!("epoch.json")).unwrap();
		for row in vectors["valid"].as_array().unwrap() {
			let text = row["text"].as_str().unwrap();
			let epoch: Epoch = text.parse().unwrap();
			assert_eq!(epoch.as_str(), text);
			assert_eq!(
				epoch.time().duration_since(UNIX_EPOCH).unwrap().as_millis(),
				row["unix_ms"].as_u64().unwrap() as u128
			);
		}
		for row in vectors["invalid"].as_array().unwrap() {
			assert!(row.as_str().unwrap().parse::<Epoch>().is_err(), "{row}");
		}
		let ordered: Vec<Epoch> = vectors["ordered"]
			.as_array()
			.unwrap()
			.iter()
			.map(|row| row.as_str().unwrap().parse().unwrap())
			.collect();
		assert!(ordered.windows(2).all(|pair| pair[0] < pair[1]));
	}

	#[test]
	fn bytes_roundtrip_and_refuse_other_versions() {
		let epoch = Epoch::mint();
		assert_eq!(Epoch::from_bytes(&epoch.to_bytes()).unwrap(), epoch);
		assert!(Epoch::from_bytes(uuid::Builder::from_random_bytes([7; 16]).into_uuid().as_bytes()).is_err());
		assert!(Epoch::from_bytes(&epoch.to_bytes()[..15]).is_err());
	}
}
