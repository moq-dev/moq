// Based on quinn-proto
// https://github.com/quinn-rs/quinn/blob/main/quinn-proto/src/varint.rs
// Licensed via Apache 2.0 and MIT

use std::convert::{TryFrom, TryInto};
use std::fmt;

use thiserror::Error;

use super::{Decode, DecodeError, Encode};

#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
#[error("value out of range")]
pub struct BoundsExceeded;

/// An integer less than 2^62
///
/// Values of this type are suitable for encoding as QUIC variable-length integer.
/// It would be neat if we could express to Rust that the top two bits are available for use as enum
/// discriminants
#[derive(Debug, Default, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct VarInt(u64);

impl VarInt {
	/// The largest possible value.
	pub const MAX: Self = Self((1 << 62) - 1);

	/// The smallest possible value.
	pub const ZERO: Self = Self(0);

	/// Construct a `VarInt` infallibly using the largest available type.
	/// Larger values need to use `try_from` instead.
	pub const fn from_u32(x: u32) -> Self {
		Self(x as u64)
	}

	pub const fn from_u64(x: u64) -> Option<Self> {
		if x <= Self::MAX.0 {
			Some(Self(x))
		} else {
			None
		}
	}

	pub const fn from_u128(x: u128) -> Option<Self> {
		if x <= Self::MAX.0 as u128 {
			Some(Self(x as u64))
		} else {
			None
		}
	}

	/// Extract the integer value
	pub const fn into_inner(self) -> u64 {
		self.0
	}
}

impl From<VarInt> for u64 {
	fn from(x: VarInt) -> Self {
		x.0
	}
}

impl From<VarInt> for usize {
	fn from(x: VarInt) -> Self {
		x.0 as usize
	}
}

impl From<VarInt> for u128 {
	fn from(x: VarInt) -> Self {
		x.0 as u128
	}
}

impl From<u8> for VarInt {
	fn from(x: u8) -> Self {
		Self(x.into())
	}
}

impl From<u16> for VarInt {
	fn from(x: u16) -> Self {
		Self(x.into())
	}
}

impl From<u32> for VarInt {
	fn from(x: u32) -> Self {
		Self(x.into())
	}
}

impl TryFrom<u64> for VarInt {
	type Error = BoundsExceeded;

	/// Succeeds iff `x` < 2^62
	fn try_from(x: u64) -> Result<Self, BoundsExceeded> {
		let x = Self(x);
		if x <= Self::MAX {
			Ok(x)
		} else {
			Err(BoundsExceeded)
		}
	}
}

impl TryFrom<u128> for VarInt {
	type Error = BoundsExceeded;

	/// Succeeds iff `x` < 2^62
	fn try_from(x: u128) -> Result<Self, BoundsExceeded> {
		if x <= Self::MAX.into() {
			Ok(Self(x as u64))
		} else {
			Err(BoundsExceeded)
		}
	}
}

impl TryFrom<usize> for VarInt {
	type Error = BoundsExceeded;

	/// Succeeds iff `x` < 2^62
	fn try_from(x: usize) -> Result<Self, BoundsExceeded> {
		Self::try_from(x as u64)
	}
}

impl TryFrom<VarInt> for u32 {
	type Error = BoundsExceeded;

	/// Succeeds iff `x` < 2^32
	fn try_from(x: VarInt) -> Result<Self, BoundsExceeded> {
		if x.0 <= u32::MAX.into() {
			Ok(x.0 as u32)
		} else {
			Err(BoundsExceeded)
		}
	}
}

impl TryFrom<VarInt> for u16 {
	type Error = BoundsExceeded;

	/// Succeeds iff `x` < 2^16
	fn try_from(x: VarInt) -> Result<Self, BoundsExceeded> {
		if x.0 <= u16::MAX.into() {
			Ok(x.0 as u16)
		} else {
			Err(BoundsExceeded)
		}
	}
}

impl TryFrom<VarInt> for u8 {
	type Error = BoundsExceeded;

	/// Succeeds iff `x` < 2^8
	fn try_from(x: VarInt) -> Result<Self, BoundsExceeded> {
		if x.0 <= u8::MAX.into() {
			Ok(x.0 as u8)
		} else {
			Err(BoundsExceeded)
		}
	}
}

impl fmt::Display for VarInt {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		self.0.fmt(f)
	}
}

impl<V> Decode<V> for VarInt {
	/// Decode a varint from the given reader.
	fn decode<R: bytes::Buf>(r: &mut R, _: V) -> Result<Self, DecodeError> {
		if !r.has_remaining() {
			return Err(DecodeError::Short);
		}

		let b = r.get_u8();
		let tag = b >> 6;

		let mut buf = [0u8; 8];
		buf[0] = b & 0b0011_1111;

		let x = match tag {
			0b00 => u64::from(buf[0]),
			0b01 => {
				if !r.has_remaining() {
					return Err(DecodeError::Short);
				}
				r.copy_to_slice(buf[1..2].as_mut());
				u64::from(u16::from_be_bytes(buf[..2].try_into().unwrap()))
			}
			0b10 => {
				if r.remaining() < 3 {
					return Err(DecodeError::Short);
				}
				r.copy_to_slice(buf[1..4].as_mut());
				u64::from(u32::from_be_bytes(buf[..4].try_into().unwrap()))
			}
			0b11 => {
				if r.remaining() < 7 {
					return Err(DecodeError::Short);
				}
				r.copy_to_slice(buf[1..8].as_mut());
				u64::from_be_bytes(buf)
			}
			_ => unreachable!(),
		};

		Ok(Self(x))
	}
}

impl<V> Encode<V> for VarInt {
	/// Encode a varint to the given writer.
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) {
		if self.0 < (1u64 << 6) {
			w.put_u8(self.0 as u8);
		} else if self.0 < (1u64 << 14) {
			w.put_u16((0b01 << 14) | self.0 as u16);
		} else if self.0 < (1u64 << 30) {
			w.put_u32((0b10 << 30) | self.0 as u32);
		} else {
			w.put_u64((0b11 << 62) | self.0);
		}
	}
}

impl<V> Encode<V> for u64 {
	/// Encode a varint to the given writer.
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		// TODO use VarInt everywhere instead of u64
		let v = VarInt::try_from(*self).expect("u64 too large");
		v.encode(w, version)
	}
}

impl<V> Decode<V> for u64 {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		VarInt::decode(r, version).map(|v| v.into_inner())
	}
}

impl<V> Encode<V> for usize {
	/// Encode a varint to the given writer.
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		// TODO use VarInt everywhere instead of usize
		let v = VarInt::try_from(*self).expect("usize too large");
		v.encode(w, version)
	}
}

impl<V> Decode<V> for usize {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		VarInt::decode(r, version).map(|v| v.into_inner() as usize)
	}
}

impl<V> Encode<V> for u32 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		VarInt::from(*self).encode(w, version)
	}
}

impl<V> Decode<V> for u32 {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let v = VarInt::decode(r, version)?;
		let v = v.try_into().map_err(|_| DecodeError::BoundsExceeded)?;
		Ok(v)
	}
}
