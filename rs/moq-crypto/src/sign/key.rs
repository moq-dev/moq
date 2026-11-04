//! The Ed25519 public key that names an endpoint, and its z-base-32 text.
//!
//! This builds without the `sign` feature, so a crate that only names endpoints shares the one
//! codec without `moq-net` or rind.

use std::fmt;
use std::str::FromStr;

/// Ed25519 key length in bytes, for both the secret seed and the public key.
pub(crate) const KEY_LEN: usize = 32;

/// z-base-32: most significant bit first, no padding, trailing bits must be zero.
pub(crate) static ZBASE32: std::sync::LazyLock<data_encoding::Encoding> = std::sync::LazyLock::new(|| {
	let mut spec = data_encoding::Specification::new();
	spec.symbols.push_str("ybndrfg8ejkmcpqxot1uwisza345h769");
	spec.encoding().expect("a valid z-base-32 specification")
});

/// An endpoint's Ed25519 public key, which is also its endpoint ID.
///
/// Its text form is the 52-character [z-base-32] of the key, the form pkarr and iroh's DNS
/// discovery use, which is also a valid DNS label. Parsing accepts only that form (lowercase,
/// zero trailing bits), so one key has exactly one ID to name a path by.
///
/// [z-base-32]: https://philzimmermann.com/docs/human-oriented-base-32-encoding.txt
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PublicKey([u8; KEY_LEN]);

impl PublicKey {
	/// The ID of the raw 32-byte Ed25519 public key.
	pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
		Self(bytes)
	}

	/// The raw 32-byte Ed25519 public key.
	pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
		&self.0
	}

	/// Whether `signature` is this key's Ed25519 signature over `message`.
	#[cfg(feature = "sign")]
	pub(crate) fn verify(&self, message: &[u8], signature: &[u8; crate::sign::scope::SIGNATURE_LEN]) -> bool {
		rind::signature::PublicKey::new(&rind::signature::verify::ED25519, &self.0)
			.is_ok_and(|key| key.verify(message, signature).is_ok())
	}
}

/// The text is not the 52-character z-base-32 of a 32-byte key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid key: expected 52 z-base-32 characters")]
#[non_exhaustive]
pub struct InvalidKey;

impl FromStr for PublicKey {
	type Err = InvalidKey;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		Ok(Self(decode(text)?))
	}
}

impl fmt::Display for PublicKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(&ZBASE32.encode(&self.0))
	}
}

impl fmt::Debug for PublicKey {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("PublicKey").field(&format_args!("{self}")).finish()
	}
}

#[cfg(feature = "serde")]
impl serde::Serialize for PublicKey {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.collect_str(self)
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for PublicKey {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <String as serde::Deserialize>::deserialize(deserializer)?;
		text.parse().map_err(serde::de::Error::custom)
	}
}

/// Decode exactly 52 z-base-32 characters. The alphabet is lowercase-only and nonzero trailing
/// bits are refused, so a key has one text form.
pub(crate) fn decode(text: &str) -> Result<[u8; KEY_LEN], InvalidKey> {
	let mut out = [0u8; KEY_LEN];
	match ZBASE32.decode_len(text.len()) {
		Ok(KEY_LEN) => ZBASE32.decode_mut(text.as_bytes(), &mut out).map_err(|_| InvalidKey)?,
		_ => return Err(InvalidKey),
	};
	Ok(out)
}

#[cfg(test)]
mod tests {
	use serde_json::Value;

	use super::PublicKey;

	fn vectors() -> Value {
		serde_json::from_str(include_str!("../../../../drafts/moq-sign-00.json")).unwrap()
	}

	#[test]
	fn vector_keys() {
		for row in vectors()["keys"].as_array().unwrap() {
			let id = &row["id"];
			let bytes = hex::decode(row["public"].as_str().unwrap()).unwrap();
			let endpoint: PublicKey = row["endpoint"].as_str().unwrap().parse().unwrap();
			assert_eq!(
				endpoint,
				PublicKey::from_bytes(bytes.clone().try_into().unwrap()),
				"{id}"
			);
			assert_eq!(endpoint.as_bytes().as_slice(), bytes, "{id}");
			assert_eq!(endpoint.to_string(), row["endpoint"].as_str().unwrap(), "{id}");
			#[cfg(feature = "serde")]
			{
				assert_eq!(serde_json::to_value(endpoint).unwrap(), row["endpoint"], "{id}");
				assert_eq!(
					serde_json::from_value::<PublicKey>(row["endpoint"].clone()).unwrap(),
					endpoint,
					"{id}"
				);
			}
		}
	}

	#[test]
	fn vector_invalid_ids() {
		for row in vectors()["invalid_ids"].as_array().unwrap() {
			let id = &row["id"];
			assert!(row["text"].as_str().unwrap().parse::<PublicKey>().is_err(), "{id}");
			#[cfg(feature = "serde")]
			assert!(
				serde_json::from_value::<PublicKey>(row["text"].clone()).is_err(),
				"{id}"
			);
		}
	}
}
