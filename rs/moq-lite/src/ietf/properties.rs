/// Track Properties — relay-visible metadata attached to tracks.
///
/// Draft-17 adds Track Properties to SUBSCRIBE_OK, PUBLISH, and FETCH_OK.
/// They appear after the message parameters as a sequence of Key-Value-Pairs
/// (same delta-encoded format) until the end of the message.
///
/// For now we parse and validate the structure but discard the values.
use bytes::Buf;

use crate::coding::{Decode, DecodeError};

use super::Version;

const MAX_PROPERTIES: u64 = 64;
/// Maximum byte value length per spec Section 1.4.3.
const MAX_KVP_VALUE_LEN: usize = (1 << 16) - 1;

/// Parse and discard Track Properties from the remaining bytes of a message.
///
/// Track Properties use the same Key-Value-Pair encoding as parameters:
/// delta-encoded types, even = varint value, odd = length-prefixed bytes.
///
/// Only call this for draft-17+; older drafts don't have Track Properties.
pub fn skip_properties<R: Buf>(r: &mut R, version: Version) -> Result<(), DecodeError> {
	// Track Properties only exist in draft-17+
	if version != Version::Draft17 {
		return Ok(());
	}

	let mut prev_type: u64 = 0;
	let mut i: u64 = 0;

	while r.has_remaining() {
		if i >= MAX_PROPERTIES {
			return Err(DecodeError::TooMany);
		}

		let delta = u64::decode(r, version)?;
		let abs = if i == 0 {
			delta
		} else {
			prev_type.checked_add(delta).ok_or(DecodeError::BoundsExceeded)?
		};
		prev_type = abs;
		i += 1;

		if abs % 2 == 0 {
			// Even type: single varint value
			let _ = u64::decode(r, version)?;
		} else {
			// Odd type: length-prefixed bytes
			let len = u64::decode(r, version)? as usize;
			if len > MAX_KVP_VALUE_LEN {
				return Err(DecodeError::BoundsExceeded);
			}
			if r.remaining() < len {
				return Err(DecodeError::Short);
			}
			r.advance(len);
		}
	}

	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Encode;
	use bytes::BytesMut;

	#[test]
	fn test_skip_empty_properties() {
		let mut buf = bytes::Bytes::new();
		skip_properties(&mut buf, Version::Draft17).unwrap();
	}

	#[test]
	fn test_skip_varint_property() {
		// Even type (0x02 = DELIVERY_TIMEOUT), varint value
		let mut buf = BytesMut::new();
		0x02u64.encode(&mut buf, Version::Draft17).unwrap(); // delta type
		5000u64.encode(&mut buf, Version::Draft17).unwrap(); // value
		let mut bytes = buf.freeze();
		skip_properties(&mut bytes, Version::Draft17).unwrap();
		assert!(!bytes.has_remaining());
	}

	#[test]
	fn test_skip_bytes_property() {
		// Odd type (0x0B = IMMUTABLE_PROPERTIES), length-prefixed
		let mut buf = BytesMut::new();
		0x0Bu64.encode(&mut buf, Version::Draft17).unwrap(); // delta type
		3u64.encode(&mut buf, Version::Draft17).unwrap(); // length
		buf.extend_from_slice(&[0x01, 0x02, 0x03]); // value bytes
		let mut bytes = buf.freeze();
		skip_properties(&mut bytes, Version::Draft17).unwrap();
		assert!(!bytes.has_remaining());
	}

	#[test]
	fn test_skip_multiple_properties() {
		let mut buf = BytesMut::new();
		// First: type 0x02 (even), varint value
		0x02u64.encode(&mut buf, Version::Draft17).unwrap();
		1000u64.encode(&mut buf, Version::Draft17).unwrap();
		// Second: delta = 0x02 → abs type 0x04 (even), varint value
		0x02u64.encode(&mut buf, Version::Draft17).unwrap();
		2000u64.encode(&mut buf, Version::Draft17).unwrap();
		// Third: delta = 0x07 → abs type 0x0B (odd), length-prefixed
		0x07u64.encode(&mut buf, Version::Draft17).unwrap();
		2u64.encode(&mut buf, Version::Draft17).unwrap();
		buf.extend_from_slice(&[0xAA, 0xBB]);

		let mut bytes = buf.freeze();
		skip_properties(&mut bytes, Version::Draft17).unwrap();
		assert!(!bytes.has_remaining());
	}
}
