//! Variable-length integers: QUIC's two-bit length tag, and moq-transport's leading ones.
//!
//! A varint is a wire encoding of a plain `u64`, not a type. The codec works on the two
//! `u32` halves so it never needs 64-bit bitwise math, which a JavaScript `number`
//! cannot do.

use thiserror::Error;

use super::{DecodeError, EncodeError};
use crate::{Version, ietf, lite};

/// The number does not fit the target: a varint wire form or a narrower integer.
#[derive(Debug, Copy, Clone, Eq, PartialEq, Error)]
#[error("value out of range")]
pub struct BoundsExceeded;

/// The largest value the QUIC form can carry: `2^62 - 1`.
pub const MAX_QUIC: u64 = (1 << 62) - 1;

/// How a protocol version lays out a varint on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Form {
	/// QUIC's two-bit length tag, up to [`MAX_QUIC`].
	Quic,
	/// Leading one bits count the length, up to `u64::MAX`.
	LeadingOnes {
		/// Whether the 7-byte form (`1111110x`) is accepted on decode, which draft-17 forbids.
		seven: bool,
	},
}

impl From<lite::Version> for Form {
	fn from(version: lite::Version) -> Self {
		match version {
			lite::Version::Lite01
			| lite::Version::Lite02
			| lite::Version::Lite03
			| lite::Version::Lite04
			| lite::Version::Lite05
			| lite::Version::Lite06
			| lite::Version::Lite07 => Self::Quic,
		}
	}
}

impl From<ietf::Version> for Form {
	fn from(version: ietf::Version) -> Self {
		match version {
			ietf::Version::Draft14 | ietf::Version::Draft15 | ietf::Version::Draft16 => Self::Quic,
			ietf::Version::Draft17 => Self::LeadingOnes { seven: false },
			_ => Self::LeadingOnes { seven: true },
		}
	}
}

impl From<Version> for Form {
	fn from(version: Version) -> Self {
		match version {
			Version::Lite(v) => v.into(),
			Version::Ietf(v) => v.into(),
		}
	}
}

/// The high and low 32 bits: the only place the codec splits a `u64`.
const fn to_halves(value: u64) -> (u32, u32) {
	((value >> 32) as u32, value as u32)
}

/// The inverse of [`to_halves`].
const fn from_halves(hi: u32, lo: u32) -> u64 {
	((hi as u64) << 32) | lo as u64
}

/// The bytes `value` takes on the wire in `form`, or [`BoundsExceeded`] if it does not fit.
pub(crate) fn size(value: u64, form: Form) -> Result<usize, BoundsExceeded> {
	let (hi, lo) = to_halves(value);
	Ok(match form {
		Form::Quic if hi == 0 && lo < 1 << 6 => 1,
		Form::Quic if hi == 0 && lo < 1 << 14 => 2,
		Form::Quic if hi == 0 && lo < 1 << 30 => 4,
		Form::Quic if hi < 1 << 30 => 8,
		Form::Quic => return Err(BoundsExceeded),
		Form::LeadingOnes { .. } if hi == 0 && lo < 1 << 7 => 1,
		Form::LeadingOnes { .. } if hi == 0 && lo < 1 << 14 => 2,
		Form::LeadingOnes { .. } if hi == 0 && lo < 1 << 21 => 3,
		Form::LeadingOnes { .. } if hi == 0 && lo < 1 << 28 => 4,
		Form::LeadingOnes { .. } if hi < 1 << 3 => 5,
		Form::LeadingOnes { .. } if hi < 1 << 10 => 6,
		// The 7-byte form is skipped: one byte longer, but legal on every draft.
		Form::LeadingOnes { .. } if hi < 1 << 24 => 8,
		Form::LeadingOnes { .. } => 9,
	})
}

/// Append the minimal encoding of `value` in `form`.
///
/// Fails past [`MAX_QUIC`] in the QUIC form, writing nothing.
#[cfg_attr(target_arch = "wasm32", inline)]
#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
pub(super) fn write(value: u64, form: Form, out: &mut Vec<u8>) -> Result<(), BoundsExceeded> {
	match form {
		Form::Quic => write_quic(value, out),
		Form::LeadingOnes { .. } => {
			write_leading_ones(value, out);
			Ok(())
		}
	}
}

// Each arm below is a fixed-size write or read, which is what keeps the codec as fast as
// a hand-rolled `put_u16`/`get_u32`. Natively the varint path is `inline(always)` from
// `Encoder::varint`/`Decoder::varint` down: left to LLVM's heuristics it stays a call
// whose `Result<u64, DecodeError>` goes through memory, which doubles the cost of a varint
// in a tight loop. wasm32 builds optimize for size, where forcing it grew moq-wasm ~10%
// gzipped, so they keep the heuristics. The QUIC read is an if-chain rather than a `match`
// on the tag: the jump table measured ~35% slower on a mixed-length stream.

#[cfg_attr(target_arch = "wasm32", inline)]
#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
fn write_quic(value: u64, out: &mut Vec<u8>) -> Result<(), BoundsExceeded> {
	let (hi, lo) = to_halves(value);
	if hi == 0 && lo < 1 << 6 {
		out.push(lo as u8);
	} else if hi == 0 && lo < 1 << 14 {
		out.extend_from_slice(&(0x4000 | lo as u16).to_be_bytes());
	} else if hi == 0 && lo < 1 << 30 {
		out.extend_from_slice(&(0x8000_0000 | lo).to_be_bytes());
	} else if hi < 1 << 30 {
		let [a, b, c, d] = (0xc000_0000 | hi).to_be_bytes();
		let [e, f, g, h] = lo.to_be_bytes();
		out.extend_from_slice(&[a, b, c, d, e, f, g, h]);
	} else {
		return Err(BoundsExceeded);
	}
	Ok(())
}

#[cfg_attr(target_arch = "wasm32", inline)]
#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
fn write_leading_ones(value: u64, out: &mut Vec<u8>) {
	let (hi, lo) = to_halves(value);
	let [a, b, c, d] = lo.to_be_bytes();
	if hi == 0 && lo < 1 << 7 {
		out.push(d);
	} else if hi == 0 && lo < 1 << 14 {
		out.extend_from_slice(&[0x80 | c, d]);
	} else if hi == 0 && lo < 1 << 21 {
		out.extend_from_slice(&[0xc0 | b, c, d]);
	} else if hi == 0 && lo < 1 << 28 {
		out.extend_from_slice(&[0xe0 | a, b, c, d]);
	} else if hi < 1 << 3 {
		out.extend_from_slice(&[0xf0 | hi as u8, a, b, c, d]);
	} else if hi < 1 << 10 {
		out.extend_from_slice(&[0xf8 | (hi >> 8) as u8, hi as u8, a, b, c, d]);
	} else if hi < 1 << 24 {
		// The 7-byte form is skipped: one byte longer, but legal on every draft.
		let [_, f, g, h] = hi.to_be_bytes();
		out.extend_from_slice(&[0xfe, f, g, h, a, b, c, d]);
	} else {
		let [e, f, g, h] = hi.to_be_bytes();
		out.extend_from_slice(&[0xff, e, f, g, h, a, b, c, d]);
	}
}

/// Decode a varint in `form` from the front of `buf`, returning it and the rest of `buf`.
#[cfg_attr(target_arch = "wasm32", inline)]
#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
pub(super) fn read(buf: &[u8], form: Form) -> Result<(u64, &[u8]), DecodeError> {
	match form {
		Form::Quic => read_quic(buf),
		Form::LeadingOnes { seven } => read_leading_ones(buf, seven),
	}
}

#[cfg_attr(target_arch = "wasm32", inline)]
#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
fn read_quic(buf: &[u8]) -> Result<(u64, &[u8]), DecodeError> {
	let Some((&first, rest)) = buf.split_first() else {
		return Err(DecodeError::Short);
	};

	let be = u32::from_be_bytes;
	if first < 0x40 {
		Ok((first as u64, rest))
	} else if first < 0x80 {
		let ([a, b], rest) = buf.split_first_chunk().ok_or(DecodeError::Short)?;
		Ok((be([0, 0, a & 0x3f, *b]) as u64, rest))
	} else if first < 0xc0 {
		let ([a, b, c, d], rest) = buf.split_first_chunk().ok_or(DecodeError::Short)?;
		Ok((be([a & 0x3f, *b, *c, *d]) as u64, rest))
	} else {
		let ([a, b, c, d, lo @ ..], rest) = buf.split_first_chunk::<8>().ok_or(DecodeError::Short)?;
		Ok((from_halves(be([a & 0x3f, *b, *c, *d]), be(*lo)), rest))
	}
}

#[cfg_attr(target_arch = "wasm32", inline)]
#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
fn read_leading_ones(buf: &[u8], seven: bool) -> Result<(u64, &[u8]), DecodeError> {
	let Some((&first, rest)) = buf.split_first() else {
		return Err(DecodeError::Short);
	};

	let be = u32::from_be_bytes;
	Ok(match first.leading_ones() {
		0 => (first as u64, rest),
		1 => {
			let ([a, b], rest) = buf.split_first_chunk().ok_or(DecodeError::Short)?;
			(be([0, 0, a & 0x3f, *b]) as u64, rest)
		}
		2 => {
			let ([a, b, c], rest) = buf.split_first_chunk().ok_or(DecodeError::Short)?;
			(be([0, a & 0x1f, *b, *c]) as u64, rest)
		}
		3 => {
			let ([a, b, c, d], rest) = buf.split_first_chunk().ok_or(DecodeError::Short)?;
			(be([a & 0x0f, *b, *c, *d]) as u64, rest)
		}
		4 => {
			let ([a, lo @ ..], rest) = buf.split_first_chunk::<5>().ok_or(DecodeError::Short)?;
			(from_halves((a & 0x07) as u32, be(*lo)), rest)
		}
		5 => {
			let ([a, b, lo @ ..], rest) = buf.split_first_chunk::<6>().ok_or(DecodeError::Short)?;
			(from_halves(be([0, 0, a & 0x03, *b]), be(*lo)), rest)
		}
		// 1111110x: the 7-byte form, which draft-17 forbids.
		6 if !seven => return Err(DecodeError::InvalidValue),
		6 => {
			let ([a, b, c, lo @ ..], rest) = buf.split_first_chunk::<7>().ok_or(DecodeError::Short)?;
			(from_halves(be([0, a & 0x01, *b, *c]), be(*lo)), rest)
		}
		7 => {
			let ([_, b, c, d, lo @ ..], rest) = buf.split_first_chunk::<8>().ok_or(DecodeError::Short)?;
			(from_halves(be([0, *b, *c, *d]), be(*lo)), rest)
		}
		_ => {
			let ([_, hi @ .., e, f, g, h], rest) = buf.split_first_chunk::<9>().ok_or(DecodeError::Short)?;
			(from_halves(be(*hi), be([*e, *f, *g, *h])), rest)
		}
	})
}

/// Decode a QUIC varint (two-bit length tag) from the front of `r`.
pub fn decode_quic<R: bytes::Buf>(r: &mut R) -> Result<u64, DecodeError> {
	let Some(&first) = r.chunk().first() else {
		return Err(DecodeError::Short);
	};

	// Copy out so a varint split across chunks still decodes.
	let len = 1usize << (first >> 6);
	if r.remaining() < len {
		return Err(DecodeError::Short);
	}
	let mut buf = [0u8; 8];
	r.copy_to_slice(&mut buf[..len]);

	Ok(read_quic(&buf[..len])?.0)
}

/// Encode `value` as a QUIC varint (two-bit length tag).
///
/// Fails with [`EncodeError::BoundsExceeded`] past [`MAX_QUIC`], writing nothing.
pub fn encode_quic<W: bytes::BufMut>(value: u64, w: &mut W) -> Result<(), EncodeError> {
	let len = size(value, Form::Quic)?;
	if w.remaining_mut() < len {
		return Err(EncodeError::Short);
	}

	let (hi, lo) = to_halves(value);
	match len {
		1 => w.put_u8(lo as u8),
		2 => w.put_u16(0x4000 | lo as u16),
		4 => w.put_u32(0x8000_0000 | lo),
		_ => {
			w.put_u32(0xc000_0000 | hi);
			w.put_u32(lo);
		}
	}
	Ok(())
}

/// Map a signed value onto the unsigned range: `(n << 1) ^ (n >> 63)`.
///
/// Small magnitudes stay small (-1 -> 1, 1 -> 2, -2 -> 3, ...), and all of `i64` fits.
pub(crate) const fn zigzag(signed: i64) -> u64 {
	((signed << 1) ^ (signed >> 63)) as u64
}

/// The inverse of [`zigzag`].
pub(crate) const fn unzigzag(value: u64) -> i64 {
	((value >> 1) as i64) ^ -((value & 1) as i64)
}

#[cfg(test)]
mod tests {
	use super::*;

	const DRAFT17: Form = Form::LeadingOnes { seven: false };
	const DRAFT18: Form = Form::LeadingOnes { seven: true };

	fn encode(value: u64, form: Form) -> Result<Vec<u8>, BoundsExceeded> {
		let mut buf = Vec::new();
		write(value, form, &mut buf)?;
		assert_eq!(buf.len(), size(value, form)?, "size disagrees with the encoding");
		Ok(buf)
	}

	/// Test vectors from the draft-17 spec (Table 2: Example Integer Encodings),
	/// excluding the known-buggy example 4 (0xdd7f3e7d).
	#[test]
	fn leading_ones_spec_examples() {
		let cases: &[(&[u8], u64)] = &[
			(&[0x25], 37),
			(&[0x80, 0x25], 37),
			(&[0xbb, 0xbd], 15_293),
			// Example 4 (0xdd7f3e7d = 494,878,333) is omitted. The spec has a bug.
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
			let (decoded, rest) = read(bytes, DRAFT17).expect("decode should succeed");
			assert_eq!(decoded, *expected, "decode mismatch for bytes {bytes:02x?}");
			assert!(rest.is_empty(), "all bytes should be consumed for {bytes:02x?}");

			// Skip the non-minimal encoding (0x8025 for 37); we only emit the minimal one.
			if bytes.len() == 1 || *expected != 37 {
				let encoded = encode(*expected, DRAFT17).unwrap();
				assert_eq!(&encoded, bytes, "encode mismatch for value {expected}");
			}
		}
	}

	/// 11111100 (0xFC) is an invalid code point on draft-17 (allowed as 7-byte form on draft-18+).
	#[test]
	fn leading_ones_invalid_0xfc() {
		assert!(
			matches!(read(&[0xFC], DRAFT17), Err(DecodeError::InvalidValue)),
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
			let encoded = encode(value, DRAFT17).unwrap();
			assert_eq!(
				encoded.len(),
				expected_len,
				"unexpected encoded length for value {value}"
			);

			let (decoded, _) = read(&encoded, DRAFT17).expect("leading-ones decode should succeed");
			assert_eq!(decoded, value, "round-trip mismatch for value {value}");
		}
	}

	/// Every length class of both forms survives a round trip, including the boundaries
	/// of each class.
	#[test]
	fn every_length_round_trips() {
		for bits in 0..64 {
			for value in [1u64 << bits, (1u64 << bits) - 1, (1u64 << bits) + 1] {
				for form in [Form::Quic, DRAFT17, DRAFT18] {
					let Ok(encoded) = encode(value, form) else {
						assert_eq!(form, Form::Quic);
						assert!(value > MAX_QUIC);
						continue;
					};
					let (decoded, rest) = read(&encoded, form).unwrap();
					assert_eq!((decoded, rest), (value, &[][..]), "{form:?} {value}");
				}
			}
		}
	}

	/// The QUIC form stops at 2^62 - 1 and refuses anything past it rather than
	/// truncating, while the leading-ones form carries the whole u64.
	#[test]
	fn quic_refuses_past_62_bits() {
		assert_eq!(encode(MAX_QUIC, Form::Quic).unwrap(), [0xff; 8]);

		for value in [1u64 << 62, u64::MAX] {
			assert_eq!(encode(value, Form::Quic), Err(BoundsExceeded));
			assert!(matches!(
				encode_quic(value, &mut Vec::new()),
				Err(EncodeError::BoundsExceeded)
			));
		}

		for value in [MAX_QUIC, 1u64 << 62, u64::MAX] {
			let encoded = encode(value, DRAFT18).unwrap();
			assert_eq!(encoded.len(), 9);
			assert_eq!(read(&encoded, DRAFT18).unwrap().0, value);
		}
	}

	#[test]
	fn draft17_rejects_7_byte_varint() {
		// 1111110x prefix: invalid on draft-17.
		let err = read(&[0xFC, 0, 0, 0, 0, 0, 0], DRAFT17).unwrap_err();
		assert!(matches!(err, DecodeError::InvalidValue));
	}

	#[test]
	fn zigzag_roundtrip_small() {
		for n in [-3i64, -2, -1, 0, 1, 2, 3, 100, -100] {
			assert_eq!(unzigzag(zigzag(n)), n, "roundtrip failed for {}", n);
		}
	}

	#[test]
	fn zigzag_small_values_compact() {
		// First few values should fit in 1 byte (varint range 0..=63 = top-2-bits tag 00).
		assert_eq!([0, -1, 1, -2, 2].map(zigzag), [0, 1, 2, 3, 4]);
	}

	/// Zigzag covers the whole i64 range; only the QUIC form bounds what goes on the wire.
	#[test]
	fn zigzag_roundtrip_boundary() {
		let mid = (1i64 << 30) + 17;

		for n in [i64::MAX, i64::MIN, (1i64 << 61) - 1, -(1i64 << 61), mid, -mid] {
			assert_eq!(unzigzag(zigzag(n)), n);
		}

		assert_eq!(zigzag(i64::MIN), u64::MAX);
		assert!(zigzag(1i64 << 61) > MAX_QUIC);
		assert_eq!(zigzag(-(1i64 << 61)), MAX_QUIC);
	}

	#[test]
	fn zigzag_quic_varint_roundtrip() {
		// Encode a zigzag value through the QUIC varint wire format.
		for n in [-5000i64, 0, 100, -1, 1_000_000, -1_000_000] {
			let bytes = encode(zigzag(n), Form::Quic).unwrap();
			let (decoded, _) = read(&bytes, Form::Quic).unwrap();
			assert_eq!(unzigzag(decoded), n);
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
		let (decoded, _) = read(&bytes, DRAFT18).unwrap();
		assert_eq!(decoded, value);
	}

	/// The Buf-based helpers other crates use read and write the same bytes, even when
	/// the varint straddles two chunks.
	#[test]
	fn quic_helpers_match_the_codec() {
		use bytes::Buf;

		let value = 0x1234_5678u64;
		let mut out = Vec::new();
		encode_quic(value, &mut out).unwrap();
		assert_eq!(out, encode(value, Form::Quic).unwrap());

		let mut split = (&out[..2]).chain(&out[2..]);
		assert_eq!(decode_quic(&mut split).unwrap(), value);
		assert!(!split.has_remaining());
	}
}
