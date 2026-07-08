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

	/// Encode a signed `i64` as a zigzag-then-unsigned varint: `(n << 1) ^ (n >> 63)`.
	///
	/// Small negative numbers map to small unsigneds (-1 -> 1, 1 -> 2, -2 -> 3, ...).
	/// Returns [`BoundsExceeded`] if `signed` is outside `[-2^61, 2^61 - 1]`, since the
	/// zigzag-encoded result must fit in a 62-bit varint.
	pub const fn from_zigzag(signed: i64) -> Result<Self, BoundsExceeded> {
		const RANGE: i64 = 1 << 61;
		if signed < -RANGE || signed >= RANGE {
			return Err(BoundsExceeded);
		}
		Ok(Self(((signed << 1) ^ (signed >> 63)) as u64))
	}

	/// Decode this varint as a signed `i64` via the inverse zigzag transform.
	pub const fn to_zigzag(self) -> i64 {
		let v = self.0;
		((v >> 1) as i64) ^ -((v & 1) as i64)
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
	pub fn decode_quic<R: bytes::Buf>(r: &mut R) -> Result<Self, DecodeError> {
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
	pub fn encode_quic<W: bytes::BufMut>(&self, w: &mut W) -> Result<(), EncodeError> {
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

	/// Decode a leading-1-bits varint (draft-17+ Section 1.4.1).
	///
	/// The number of leading 1-bits determines the byte length:
	/// - `0xxxxxxx` → 1 byte, 7 usable bits
	/// - `10xxxxxx` → 2 bytes, 14 usable bits
	/// - `110xxxxx` → 3 bytes, 21 usable bits
	/// - `1110xxxx` → 4 bytes, 28 usable bits
	/// - `11110xxx` → 5 bytes, 35 usable bits
	/// - `111110xx` → 6 bytes, 42 usable bits
	/// - `1111110x` → 7 bytes, 49 usable bits (draft-18+, INVALID in draft-17 per #1595)
	/// - `11111110` → 8 bytes, 56 usable bits
	/// - `11111111` → 9 bytes, 64 usable bits
	fn decode_leading_ones<R: bytes::Buf>(r: &mut R, version: ietf::Version) -> Result<Self, DecodeError> {
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
				// 1111110x + 6 bytes, 49 bits (draft-18+, INVALID in draft-17 per #1595)
				if matches!(version, ietf::Version::Draft17) {
					return Err(DecodeError::InvalidValue);
				}
				if r.remaining() < 6 {
					return Err(DecodeError::Short);
				}
				let hi = u64::from(b & 0x01);
				let mut buf = [0u8; 8];
				r.copy_to_slice(&mut buf[2..]);
				Ok(Self((hi << 48) | u64::from_be_bytes(buf)))
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

	/// Encode a leading-1-bits varint (draft-17+ Section 1.4.1).
	///
	/// Always emits the minimal canonical form. Draft-18 also accepts 7-byte form
	/// (`1111110x`) on decode but we never emit it because the 8-byte form is one byte
	/// larger but simpler and is universally valid.
	fn encode_leading_ones<W: bytes::BufMut>(&self, w: &mut W, _version: ietf::Version) -> Result<(), EncodeError> {
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

use crate::{Version, ietf, lite};

// All lite versions use QUIC-style varint encoding.
impl Encode<lite::Version> for VarInt {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: lite::Version) -> Result<(), EncodeError> {
		self.encode_quic(w)
	}
}

impl Decode<lite::Version> for VarInt {
	fn decode<R: bytes::Buf>(r: &mut R, _: lite::Version) -> Result<Self, DecodeError> {
		Self::decode_quic(r)
	}
}

// Draft14-16 use QUIC-style varints; draft-17+ uses leading-ones.
impl Encode<ietf::Version> for VarInt {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: ietf::Version) -> Result<(), EncodeError> {
		match version {
			ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16 => self.encode_quic(w),
			_ => self.encode_leading_ones(w, version),
		}
	}
}

impl Decode<ietf::Version> for VarInt {
	fn decode<R: bytes::Buf>(r: &mut R, version: ietf::Version) -> Result<Self, DecodeError> {
		match version {
			ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16 => Self::decode_quic(r),
			_ => Self::decode_leading_ones(r, version),
		}
	}
}

// The top-level Version delegates to the sub-version impls.
impl Encode<Version> for VarInt {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite(v) => self.encode(w, v),
			Version::Ietf(v) => self.encode(w, v),
		}
	}
}

impl Decode<Version> for VarInt {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite(v) => Self::decode(r, v),
			Version::Ietf(v) => Self::decode(r, v),
		}
	}
}

// Blanket impls for integer types that delegate to VarInt.
impl<V: Copy> Encode<V> for u64
where
	VarInt: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		VarInt::try_from(*self)?.encode(w, version)
	}
}

impl<V: Copy> Decode<V> for u64
where
	VarInt: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		VarInt::decode(r, version).map(|v| v.into_inner())
	}
}

impl<V: Copy> Encode<V> for usize
where
	VarInt: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		VarInt::try_from(*self)?.encode(w, version)
	}
}

impl<V: Copy> Decode<V> for usize
where
	VarInt: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		VarInt::decode(r, version).map(|v| v.into_inner() as usize)
	}
}

impl<V: Copy> Encode<V> for u32
where
	VarInt: Encode<V>,
{
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		VarInt::from(*self).encode(w, version)
	}
}

impl<V: Copy> Decode<V> for u32
where
	VarInt: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let v = VarInt::decode(r, version)?;
		let v = v.try_into().map_err(|_| DecodeError::BoundsExceeded)?;
		Ok(v)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{ietf, lite};
	use bytes::Bytes;

	/// Test vectors from the draft-17 spec (Table 2: Example Integer Encodings),
	/// excluding the known-buggy example 4 (0xdd7f3e7d).
	#[test]
	fn leading_ones_spec_examples() {
		let cases: &[(&[u8], u64)] = &[
			(&[0x25], 37),
			(&[0x80, 0x25], 37),
			(&[0xbb, 0xbd], 15_293),
			// Example 4 (0xdd7f3e7d = 494,878,333) is omitted — the spec has a bug.
			// See https://github.com/moq-wg/moq-transport/pull/1521
			(&[0xfa, 0xa1, 0xa0, 0xe4, 0x03, 0xd8], 2_893_212_287_960),
			(
				&[0xfe, 0xfa, 0x31, 0x8f, 0xa8, 0xe3, 0xca, 0x11],
				70_423_237_261_249_041,
			),
			(
				&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
				18_446_744_073_709_551_615,
			),
		];

		for (bytes, expected) in cases {
			// Test decoding
			let mut buf = Bytes::from(bytes.to_vec());
			let decoded = VarInt::decode_leading_ones(&mut buf, ietf::Version::Draft17).expect("decode should succeed");
			assert_eq!(
				decoded.into_inner(),
				*expected,
				"decode mismatch for bytes {bytes:02x?}"
			);
			assert_eq!(buf.len(), 0, "all bytes should be consumed for {bytes:02x?}");

			// Test round-trip encode:
			// - Skip non-minimal encoding (0x8025 for 37)
			// - Skip u64::MAX which exceeds VarInt::MAX (2^62-1) but is decodable
			if let Some(varint) = VarInt::from_u64(*expected)
				&& (bytes.len() == 1 || *expected != 37)
			{
				let mut encoded = Vec::new();
				varint
					.encode_leading_ones(&mut encoded, ietf::Version::Draft17)
					.expect("encode should succeed");
				assert_eq!(&encoded, bytes, "encode mismatch for value {expected}");
			}
		}
	}

	/// 11111100 (0xFC) is an invalid code point on draft-17 (allowed as 7-byte form on draft-18+).
	#[test]
	fn leading_ones_invalid_0xfc() {
		let mut buf = Bytes::from_static(&[0xFC]);
		assert!(
			matches!(
				VarInt::decode_leading_ones(&mut buf, ietf::Version::Draft17),
				Err(DecodeError::InvalidValue)
			),
			"0xFC should be rejected as invalid on draft-17"
		);
	}

	#[test]
	fn leading_ones_boundaries_round_trip() {
		let cases = [
			((1u64 << 7) - 1, 1usize),
			(1u64 << 7, 2usize),
			((1u64 << 14) - 1, 2usize),
			(1u64 << 14, 3usize),
			((1u64 << 56) - 1, 8usize),
			(1u64 << 56, 9usize),
		];

		for (value, expected_len) in cases {
			let varint = VarInt::from_u64(value).expect("value should be representable as VarInt");
			let mut encoded = Vec::new();
			varint
				.encode_leading_ones(&mut encoded, ietf::Version::Draft17)
				.expect("leading-ones encode should succeed");
			assert_eq!(
				encoded.len(),
				expected_len,
				"unexpected encoded length for value {value}"
			);

			let mut bytes = Bytes::from(encoded);
			let decoded = VarInt::decode_leading_ones(&mut bytes, ietf::Version::Draft17)
				.expect("leading-ones decode should succeed");
			assert_eq!(decoded.into_inner(), value, "round-trip mismatch for value {value}");
		}
	}

	#[test]
	fn draft17_rejects_7_byte_varint() {
		// 1111110x prefix: invalid on draft-17.
		let bytes = Bytes::from(vec![0xFC, 0, 0, 0, 0, 0, 0]);
		let mut buf = bytes.clone();
		let err = VarInt::decode_leading_ones(&mut buf, ietf::Version::Draft17).unwrap_err();
		assert!(matches!(err, DecodeError::InvalidValue));
	}

	#[test]
	fn zigzag_roundtrip_small() {
		for n in [-3i64, -2, -1, 0, 1, 2, 3, 100, -100] {
			let v = VarInt::from_zigzag(n).unwrap();
			assert_eq!(v.to_zigzag(), n, "roundtrip failed for {}", n);
		}
	}

	#[test]
	fn zigzag_small_values_compact() {
		// First few values should fit in 1 byte (varint range 0..=63 = top-2-bits tag 00).
		assert_eq!(VarInt::from_zigzag(0).unwrap().into_inner(), 0);
		assert_eq!(VarInt::from_zigzag(-1).unwrap().into_inner(), 1);
		assert_eq!(VarInt::from_zigzag(1).unwrap().into_inner(), 2);
		assert_eq!(VarInt::from_zigzag(-2).unwrap().into_inner(), 3);
		assert_eq!(VarInt::from_zigzag(2).unwrap().into_inner(), 4);
	}

	#[test]
	fn zigzag_roundtrip_boundary() {
		// Boundary values in the valid input range [-2^61, 2^61 - 1].
		let max = (1i64 << 61) - 1;
		let min = -(1i64 << 61);
		let mid = (1i64 << 30) + 17;

		for n in [max, min, mid, -mid] {
			let v = VarInt::from_zigzag(n).unwrap();
			assert_eq!(v.to_zigzag(), n);
		}
	}

	#[test]
	fn zigzag_out_of_range_rejected() {
		// Values past the i61 boundary are out of varint range.
		assert!(VarInt::from_zigzag(1i64 << 61).is_err());
		assert!(VarInt::from_zigzag(-(1i64 << 61) - 1).is_err());
		assert!(VarInt::from_zigzag(i64::MAX).is_err());
		assert!(VarInt::from_zigzag(i64::MIN).is_err());
	}

	#[test]
	fn zigzag_quic_varint_roundtrip() {
		// Encode a zigzag value through the QUIC varint wire format.
		for n in [-5000i64, 0, 100, -1, 1_000_000, -1_000_000] {
			let v = VarInt::from_zigzag(n).unwrap();

			let mut buf = bytes::BytesMut::new();
			v.encode(&mut buf, lite::Version::Lite01).unwrap();
			let mut bytes = buf.freeze();
			let decoded = VarInt::decode(&mut bytes, lite::Version::Lite01).unwrap();
			assert_eq!(decoded.to_zigzag(), n);
		}
	}

	#[test]
	fn draft18_accepts_7_byte_varint() {
		// Value 0x1234_5678_9ABC encoded as 7-byte leading-ones (1111110x | hi, +6 bytes).
		let value: u64 = 0x1234_5678_9ABC;
		let mut bytes = Vec::new();
		// Prefix byte: 1111110_0 + (value >> 48) bit. Top 1 bit of 49 = bit 48.
		// value fits in 49 bits, so the 0x01 LSB of prefix encodes bit 48 of value.
		let hi_bit = ((value >> 48) & 0x01) as u8;
		bytes.push(0xFC | hi_bit);
		for shift in (0..48).step_by(8).rev() {
			bytes.push(((value >> shift) & 0xFF) as u8);
		}
		let mut buf = Bytes::from(bytes);
		let decoded = VarInt::decode_leading_ones(&mut buf, ietf::Version::Draft18).unwrap();
		assert_eq!(decoded.into_inner(), value);
	}
}
