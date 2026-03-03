// Based on quinn-proto
// https://github.com/quinn-rs/quinn/blob/main/quinn-proto/src/varint.rs
// Licensed via Apache 2.0 and MIT

use std::convert::{TryFrom, TryInto};
use std::fmt;

use thiserror::Error;

use super::{Decode, DecodeError, Encode, EncodeError};

/// The number is too large to fit in a VarInt (62 bits).
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
		if x <= Self::MAX.0 { Some(Self(x)) } else { None }
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
		if x <= Self::MAX { Ok(x) } else { Err(BoundsExceeded) }
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

impl VarInt {
	/// Decode a QUIC-style varint (2-bit length tag in top bits).
	fn decode_quic<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
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

	/// Encode a QUIC-style varint (2-bit length tag in top bits).
	fn encode_quic<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
		let remaining = w.remaining_mut();
		if self.0 < (1u64 << 6) {
			if remaining < 1 {
				return Err(EncodeError::Short);
			}
			w.put_u8(self.0 as u8);
		} else if self.0 < (1u64 << 14) {
			if remaining < 2 {
				return Err(EncodeError::Short);
			}
			w.put_u16((0b01 << 14) | self.0 as u16);
		} else if self.0 < (1u64 << 30) {
			if remaining < 4 {
				return Err(EncodeError::Short);
			}
			w.put_u32((0b10 << 30) | self.0 as u32);
		} else if self.0 < (1u64 << 62) {
			if remaining < 8 {
				return Err(EncodeError::Short);
			}
			w.put_u64((0b11 << 62) | self.0);
		} else {
			return Err(BoundsExceeded.into());
		}
		Ok(())
	}

	/// Decode a leading-1-bits varint (draft-17 Section 1.4.1).
	///
	/// The number of leading 1-bits determines the byte length:
	/// - `0xxxxxxx` → 1 byte, 7 usable bits
	/// - `10xxxxxx` → 2 bytes, 14 usable bits
	/// - `110xxxxx` → 3 bytes, 21 usable bits
	/// - `1110xxxx` → 4 bytes, 28 usable bits
	/// - `11110xxx` → 5 bytes, 35 usable bits
	/// - `111110xx` → 6 bytes, 42 usable bits
	/// - `11111110` → 8 bytes, 56 usable bits (skips 7)
	/// - `11111111` → 9 bytes, 64 usable bits
	fn decode_leading_ones<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
		if !r.has_remaining() {
			return Err(DecodeError::Short);
		}

		let b = r.get_u8();
		let ones = b.leading_ones() as usize;

		match ones {
			0 => {
				// 0xxxxxxx — 7 bits
				Ok(Self(u64::from(b)))
			}
			1 => {
				// 10xxxxxx + 1 byte — 14 bits
				if !r.has_remaining() {
					return Err(DecodeError::Short);
				}
				let hi = u64::from(b & 0x3F);
				let lo = u64::from(r.get_u8());
				Ok(Self((hi << 8) | lo))
			}
			2 => {
				// 110xxxxx + 2 bytes — 21 bits
				if r.remaining() < 2 {
					return Err(DecodeError::Short);
				}
				let hi = u64::from(b & 0x1F);
				let mut buf = [0u8; 2];
				r.copy_to_slice(&mut buf);
				Ok(Self((hi << 16) | u64::from(u16::from_be_bytes(buf))))
			}
			3 => {
				// 1110xxxx + 3 bytes — 28 bits
				if r.remaining() < 3 {
					return Err(DecodeError::Short);
				}
				let hi = u64::from(b & 0x0F);
				let mut buf = [0u8; 3];
				r.copy_to_slice(&mut buf);
				Ok(Self(
					(hi << 24) | u64::from(buf[0]) << 16 | u64::from(buf[1]) << 8 | u64::from(buf[2]),
				))
			}
			4 => {
				// 11110xxx + 4 bytes — 35 bits
				if r.remaining() < 4 {
					return Err(DecodeError::Short);
				}
				let hi = u64::from(b & 0x07);
				let mut buf = [0u8; 4];
				r.copy_to_slice(&mut buf);
				Ok(Self((hi << 32) | u64::from(u32::from_be_bytes(buf))))
			}
			5 => {
				// 111110xx + 5 bytes — 42 bits
				if r.remaining() < 5 {
					return Err(DecodeError::Short);
				}
				let hi = u64::from(b & 0x03);
				let mut buf = [0u8; 5];
				r.copy_to_slice(&mut buf);
				let lo = u64::from(buf[0]) << 32
					| u64::from(buf[1]) << 24
					| u64::from(buf[2]) << 16
					| u64::from(buf[3]) << 8
					| u64::from(buf[4]);
				Ok(Self((hi << 40) | lo))
			}
			6 => {
				// 1111110x — INVALID per draft-17
				Err(DecodeError::InvalidValue)?
			}
			7 => {
				// 11111110 + 7 bytes — 56 bits
				if r.remaining() < 7 {
					return Err(DecodeError::Short);
				}
				let mut buf = [0u8; 8];
				buf[0] = 0;
				r.copy_to_slice(&mut buf[1..]);
				Ok(Self(u64::from_be_bytes(buf)))
			}
			8 => {
				// 11111111 + 8 bytes — 64 bits
				if r.remaining() < 8 {
					return Err(DecodeError::Short);
				}
				let mut buf = [0u8; 8];
				r.copy_to_slice(&mut buf);
				Ok(Self(u64::from_be_bytes(buf)))
			}
			_ => unreachable!(),
		}
	}

	/// Encode a leading-1-bits varint (draft-17 Section 1.4.1).
	fn encode_leading_ones<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
		let x = self.0;
		let remaining = w.remaining_mut();

		if x < (1 << 7) {
			// 0xxxxxxx — 1 byte
			if remaining < 1 {
				return Err(EncodeError::Short);
			}
			w.put_u8(x as u8);
		} else if x < (1 << 14) {
			// 10xxxxxx — 2 bytes
			if remaining < 2 {
				return Err(EncodeError::Short);
			}
			w.put_u8(0x80 | (x >> 8) as u8);
			w.put_u8(x as u8);
		} else if x < (1 << 21) {
			// 110xxxxx — 3 bytes
			if remaining < 3 {
				return Err(EncodeError::Short);
			}
			w.put_u8(0xC0 | (x >> 16) as u8);
			w.put_u16(x as u16);
		} else if x < (1 << 28) {
			// 1110xxxx — 4 bytes
			if remaining < 4 {
				return Err(EncodeError::Short);
			}
			w.put_u8(0xE0 | (x >> 24) as u8);
			w.put_u8((x >> 16) as u8);
			w.put_u16(x as u16);
		} else if x < (1 << 35) {
			// 11110xxx — 5 bytes
			if remaining < 5 {
				return Err(EncodeError::Short);
			}
			w.put_u8(0xF0 | (x >> 32) as u8);
			w.put_u32(x as u32);
		} else if x < (1 << 42) {
			// 111110xx — 6 bytes
			if remaining < 6 {
				return Err(EncodeError::Short);
			}
			w.put_u8(0xF8 | (x >> 40) as u8);
			w.put_u8((x >> 32) as u8);
			w.put_u32(x as u32);
		} else if x < (1 << 56) {
			// 11111110 — 8 bytes (skips 7)
			if remaining < 8 {
				return Err(EncodeError::Short);
			}
			w.put_u8(0xFE);
			// Write 7 bytes: high byte then low 6 bytes
			w.put_u8((x >> 48) as u8);
			w.put_u16((x >> 32) as u16);
			w.put_u32(x as u32);
		} else {
			// 11111111 — 9 bytes
			if remaining < 9 {
				return Err(EncodeError::Short);
			}
			w.put_u8(0xFF);
			w.put_u64(x);
		}

		Ok(())
	}
}

use crate::Version;

impl Decode<Version> for VarInt {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft17 => Self::decode_leading_ones(r),
			Version::Lite01
			| Version::Lite02
			| Version::Lite03
			| Version::Draft14
			| Version::Draft15
			| Version::Draft16 => Self::decode_quic(r),
		}
	}
}

impl Encode<Version> for VarInt {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft17 => self.encode_leading_ones(w),
			Version::Lite01
			| Version::Lite02
			| Version::Lite03
			| Version::Draft14
			| Version::Draft15
			| Version::Draft16 => self.encode_quic(w),
		}
	}
}

impl Encode<Version> for u64 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let v = VarInt::try_from(*self)?;
		v.encode(w, version)
	}
}

impl Decode<Version> for u64 {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		VarInt::decode(r, version).map(|v| v.into_inner())
	}
}

impl Encode<Version> for usize {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let v = VarInt::try_from(*self)?;
		v.encode(w, version)
	}
}

impl Decode<Version> for usize {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		VarInt::decode(r, version).map(|v| v.into_inner() as usize)
	}
}

impl Encode<Version> for u32 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		VarInt::from(*self).encode(w, version)
	}
}

impl Decode<Version> for u32 {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let v = VarInt::decode(r, version)?;
		let v = v.try_into().map_err(|_| DecodeError::BoundsExceeded)?;
		Ok(v)
	}
}
