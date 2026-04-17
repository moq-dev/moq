use std::fmt;

use rand::Rng;

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

/// A unique identifier for an origin, encoded as a varint on the wire.
///
/// Must be a non-zero 62-bit value (1 <= value < 2^62).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct OriginId(u64);

/// The maximum valid OriginId value (2^62 - 1).
const ORIGIN_ID_MAX: u64 = (1u64 << 62) - 1;

impl OriginId {
	/// A placeholder value used when the actual OriginId is unknown (e.g., Lite03 hop placeholders).
	pub const UNKNOWN: Self = Self(0);

	/// Generate a random non-zero 62-bit origin ID.
	pub fn random() -> Self {
		let mut rng = rand::rng();
		let value = rng.random_range(1..(1u64 << 62));
		Self(value)
	}

	/// Get the inner u64 value.
	pub fn into_inner(self) -> u64 {
		self.0
	}
}

impl TryFrom<u64> for OriginId {
	type Error = InvalidOriginId;

	fn try_from(value: u64) -> Result<Self, Self::Error> {
		if value == 0 || value > ORIGIN_ID_MAX {
			return Err(InvalidOriginId(value));
		}
		Ok(Self(value))
	}
}

/// Error returned when constructing an OriginId with an invalid value.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct InvalidOriginId(pub u64);

impl fmt::Display for InvalidOriginId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "invalid OriginId: {} (must be 1 <= value < 2^62)", self.0)
	}
}

impl std::error::Error for InvalidOriginId {}

impl fmt::Display for OriginId {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

impl<V: Copy> Encode<V> for OriginId
where
	u64: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.0.encode(w, version)
	}
}

impl<V: Copy> Decode<V> for OriginId
where
	u64: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let value = u64::decode(r, version)?;
		Self::try_from(value).map_err(|_| DecodeError::InvalidValue)
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for OriginId {
	fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
	where
		D: serde::Deserializer<'de>,
	{
		let value = u64::deserialize(deserializer)?;
		Self::try_from(value).map_err(serde::de::Error::custom)
	}
}
