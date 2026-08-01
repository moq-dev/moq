/// Track Properties: relay-visible metadata attached to tracks.
///
/// Draft-17 adds Track Properties to SUBSCRIBE_OK, PUBLISH, and FETCH_OK.
/// They appear after the message parameters as a sequence of Key-Value-Pairs
/// (same delta-encoded format) until the end of the message.
///
/// Unlike Message Parameters which have a count prefix, Track Properties
/// have no count and are read until the end of the message payload.
///
/// TIMESCALE is the one property we understand; the rest are parsed and discarded.
use bytes::Buf;

use crate::Timescale;
use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::Version;

const MAX_PROPERTIES: u64 = 64;
/// Maximum byte value length per spec Section 1.4.3.
const MAX_KVP_VALUE_LEN: usize = (1 << 16) - 1;

/// TIMESCALE (0x08), from the MOQ Properties registry shared with draft-ietf-moq-loc-04.
///
/// Track scope: it declares the units of every object Timestamp on the track, and its
/// presence is what opts the track into timestamps at all.
const TIMESCALE: u64 = 0x08;

/// Write the track's Timescale as a Track Property block.
///
/// Track Properties are the final field of the message, so this writes the block with
/// no count and no length; the caller must not append anything after it.
pub fn encode<W: bytes::BufMut>(w: &mut W, timescale: Timescale, version: Version) -> Result<(), EncodeError> {
	// Track Properties only exist in draft-17+; older drafts have nowhere to put this.
	match version {
		Version::Draft14 | Version::Draft15 | Version::Draft16 => return Ok(()),
		_ => {}
	}

	// First property in the block, so the delta-encoded type is the absolute id.
	TIMESCALE.encode(w, version)?;
	u64::from(timescale).encode(w, version)
}

/// Parse Track Properties from the remaining bytes of a message, returning the track's
/// Timescale if it declared one.
///
/// Track Properties use the same Key-Value-Pair encoding as parameters:
/// delta-encoded types, even = varint value, odd = length-prefixed bytes.
/// They have no count prefix. Read until the buffer is empty.
///
/// `None` means the track declared no timeline, which is not an error: the caller
/// interprets its objects by arrival time instead. A Timescale of 0 is invalid and
/// decodes to `None` for the same reason.
///
/// Only call this for draft-17+; older drafts don't have Track Properties.
pub fn decode<R: Buf>(r: &mut R, version: Version) -> Result<Option<Timescale>, DecodeError> {
	// Track Properties only exist in draft-17+
	match version {
		Version::Draft14 | Version::Draft15 | Version::Draft16 => return Ok(None),
		_ => {}
	}

	let mut timescale = None;

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
			let value = u64::decode(r, version)?;
			if abs == TIMESCALE {
				// A zero timescale is invalid; treat it as no declaration rather than
				// failing the whole message over one property we could have ignored.
				timescale = Timescale::new(value).ok();
			}
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

	Ok(timescale)
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Encode;
	use bytes::BytesMut;

	#[test]
	fn test_skip_empty_properties() {
		let mut buf = bytes::Bytes::new();
		decode(&mut buf, Version::Draft17).unwrap();
	}

	#[test]
	fn test_skip_varint_property() {
		// Even type (0x02 = DELIVERY_TIMEOUT), varint value
		let mut buf = BytesMut::new();
		0x02u64.encode(&mut buf, Version::Draft17).unwrap(); // delta type
		5000u64.encode(&mut buf, Version::Draft17).unwrap(); // value
		let mut bytes = buf.freeze();
		decode(&mut bytes, Version::Draft17).unwrap();
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
		decode(&mut bytes, Version::Draft17).unwrap();
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
		decode(&mut bytes, Version::Draft17).unwrap();
		assert!(!bytes.has_remaining());
	}
}
