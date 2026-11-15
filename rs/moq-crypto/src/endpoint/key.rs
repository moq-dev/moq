//! The Ed25519 public key that names an endpoint, and its z-base-32 text.
//!
//! This builds without the `endpoint` feature, so a crate that only names endpoints shares the one
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
	use super::PublicKey;

	/// Public key bytes (hex) and their endpoint ID. The first is RFC 8032's test 1.
	const KEYS: [(&str, &str); 3] = [
		(
			"d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
			"47pjoycnsrfmxikm95jh13y88e8qnhzu5kungjpxyepgt7a8krpy",
		),
		(
			"ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
			"999999999999999999999999999999999999999999999999999o",
		),
		(
			"0000000000000000000000000000000000000000000000000000000000000000",
			"yyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyyy",
		),
	];

	/// Text that is not the one canonical form of any key.
	const INVALID: [&str; 11] = [
		"EYAMFTWCF3SCK8RS5M1MEUGF8ODWQG78473T7P3ST9WXOB9CSZ8Y",
		"eyamftwcf3sck8rs5m1meugf8odwqg78473t7p3st9wxob9csz8",
		"eyamftwcf3sck8rs5m1meugf8odwqg78473t7p3st9wxob9csz8yy",
		"0yamftwcf3sck8rs5m1meugf8odwqg78473t7p3st9wxob9csz8y",
		"eyamftwcf3sck8rs5m1meugf8odwqg78473t7p3st9wxob9csz8b",
		"4030b2c68c2e6cc51c96dae4b44cc53c07471ba7d7731eb7368fe8f807ecb5ce",
		"",
		"9999999999999999999999999999999999999999999999999999",
		"eyamftwcf3sck8rs5m1meugf8odwqg78473t7p3st9wxob9csz8y====",
		" eyamftwcf3sck8rs5m1meugf8odwqg78473t7p3st9wxob9csz8y",
		"eyamftwcf3sck8rs5m1meugf8odwqg78473t7p3st9wxob9csz8y\n",
	];

	#[test]
	fn keys_round_trip_their_text() {
		for (public, endpoint) in KEYS {
			let bytes: [u8; 32] = hex::decode(public).unwrap().try_into().unwrap();
			let key: PublicKey = endpoint.parse().unwrap();
			assert_eq!(key, PublicKey::from_bytes(bytes), "{endpoint}");
			assert_eq!(key.as_bytes(), &bytes, "{endpoint}");
			assert_eq!(key.to_string(), endpoint);
			#[cfg(feature = "serde")]
			{
				let text = serde_json::Value::from(endpoint);
				assert_eq!(serde_json::to_value(key).unwrap(), text, "{endpoint}");
				assert_eq!(serde_json::from_value::<PublicKey>(text).unwrap(), key, "{endpoint}");
			}
		}
	}

	#[test]
	fn invalid_text_is_refused() {
		for text in INVALID {
			assert!(text.parse::<PublicKey>().is_err(), "{text:?}");
			#[cfg(feature = "serde")]
			assert!(serde_json::from_value::<PublicKey>(text.into()).is_err(), "{text:?}");
		}
	}
}
