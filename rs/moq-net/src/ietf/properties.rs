/// Track Properties: relay-visible metadata attached to tracks.
///
/// Draft-17 adds Track Properties to SUBSCRIBE_OK, PUBLISH, and FETCH_OK.
/// They appear after the message parameters as a sequence of Key-Value-Pairs
/// (same delta-encoded format) until the end of the message.
///
/// Unlike Message Parameters which have a count prefix, Track Properties
/// have no count and are read until the end of the message payload.
///
/// MAX_CACHE_DURATION, TIMESCALE, DEFAULT_PUBLISHER_PRIORITY, and DEFAULT_PUBLISHER_GROUP_ORDER are understood;
/// the rest are parsed and discarded.
use bytes::Buf;
use std::time::Duration;

use crate::Timescale;
use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::{GroupOrder, Version};

const MAX_PROPERTIES: u64 = 64;
/// Maximum byte value length per spec Section 1.4.3.
const MAX_KVP_VALUE_LEN: usize = (1 << 16) - 1;

/// TIMESCALE (0x08), from the MOQ Properties registry shared with draft-ietf-moq-loc-04.
///
/// Track scope: it declares the units of every object Timestamp on the track, and its
/// presence is what opts the track into timestamps at all.
const TIMESCALE: u64 = 0x08;

/// DEFAULT_PUBLISHER_GROUP_ORDER (0x22), the publisher's delivery preference for the track.
///
/// It shares its number with the GROUP_ORDER *message parameter*, which is a different
/// registry: that one is only legal in SUBSCRIBE, PUBLISH_OK, and FETCH, where the
/// subscriber states its own preference.
const DEFAULT_PUBLISHER_PRIORITY: u64 = 0x0e;
const DEFAULT_PUBLISHER_GROUP_ORDER: u64 = 0x22;

/// The Track Properties block carried at the end of SUBSCRIBE_OK, PUBLISH, and FETCH_OK.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Properties {
	/// The publisher's cache window; omission leaves retention unconstrained.
	pub max_cache_duration: Option<Duration>,

	/// The track's Timescale, which declares the units of every object Timestamp on it.
	///
	/// `None` declares no timeline, so the subscriber times objects by arrival.
	pub timescale: Option<Timescale>,

	/// Publisher priority for groups whose header omits it. Draft-21 section 10.4
	/// assigns property 0x0e and defaults to wire priority 128 when absent.
	pub priority: Option<u8>,

	/// The publisher's preference for prioritizing groups within a subscription.
	///
	/// `None` means the draft default, Ascending.
	pub group_order: Option<GroupOrder>,
}

impl Properties {
	/// Write the block, which is the final field of the message: no count and no length,
	/// so the caller must not append anything after it.
	///
	/// Properties are serialized in ascending order by type, delta-encoded.
	pub fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		// Draft-16 carries the same block under the name Track Extensions, but we only write it
		// from draft-17 on: a draft-16 peer running an older build of this crate rejects any
		// trailing bytes it doesn't parse, and draft-16 never registered TIMESCALE (0x08). We
		// still read the block on draft-16, so a peer that sends one is understood.
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => return Ok(()),
			_ => {}
		}

		let mut prev_type = 0;

		if let Some(age) = self.max_cache_duration {
			4u64.encode(w, version)?;
			age.encode(w, version)?;
			prev_type = 4;
		}

		if let Some(timescale) = self.timescale {
			(TIMESCALE - prev_type).encode(w, version)?;
			u64::from(timescale).encode(w, version)?;
			prev_type = TIMESCALE;
		}

		if let Some(priority) = self.priority {
			(DEFAULT_PUBLISHER_PRIORITY - prev_type).encode(w, version)?;
			u64::from(priority).encode(w, version)?;
			prev_type = DEFAULT_PUBLISHER_PRIORITY;
		}

		if let Some(group_order) = self.group_order {
			(DEFAULT_PUBLISHER_GROUP_ORDER - prev_type).encode(w, version)?;
			u64::from(u8::from(group_order)).encode(w, version)?;
		}

		Ok(())
	}

	/// Parse Track Properties from the remaining bytes of a message.
	///
	/// Track Properties use the same Key-Value-Pair encoding as parameters:
	/// delta-encoded types, even = varint value, odd = length-prefixed bytes.
	/// They have no count prefix. Read until the buffer is empty.
	///
	/// Unlike an unknown message parameter, an unknown property is skipped rather than
	/// fatal, which is what lets a relay forward properties it does not implement.
	///
	/// Drafts before 16 have no such block, so this reads nothing and leaves the buffer alone.
	pub fn decode<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let mut properties = Self::default();

		// Draft-16 calls the block Track Extensions, draft-17+ Track Properties. Same encoding,
		// so read either rather than faulting the message for trailing bytes.
		match version {
			Version::Draft14 | Version::Draft15 => return Ok(properties),
			_ => {}
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
				let value = u64::decode(r, version)?;
				match abs {
					4 => properties.max_cache_duration = Some(Duration::from_millis(value)),
					TIMESCALE => {
						// A zero timescale is invalid; treat it as no declaration rather than
						// failing the whole message over one property we could have ignored.
						properties.timescale = Timescale::new(value).ok();
					}
					DEFAULT_PUBLISHER_PRIORITY => {
						properties.priority = Some(u8::try_from(value).map_err(|_| DecodeError::InvalidValue)?);
					}
					DEFAULT_PUBLISHER_GROUP_ORDER => {
						// Only Ascending and Descending are defined here. Unlike the draft-14
						// fields, 0x0 has no "publisher decides" meaning to fall back on.
						properties.group_order = match value {
							1 => Some(GroupOrder::Ascending),
							2 => Some(GroupOrder::Descending),
							_ => return Err(DecodeError::InvalidValue),
						};
					}
					_ => {}
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

		Ok(properties)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Encode;
	use bytes::BytesMut;

	#[test]
	fn test_skip_empty_properties() {
		let mut buf = bytes::Bytes::new();
		assert_eq!(
			Properties::decode(&mut buf, Version::Draft17).unwrap(),
			Properties::default()
		);
	}

	#[test]
	fn test_skip_varint_property() {
		// Even type (0x02 = DELIVERY_TIMEOUT), varint value
		let mut buf = BytesMut::new();
		0x02u64.encode(&mut buf, Version::Draft17).unwrap(); // delta type
		5000u64.encode(&mut buf, Version::Draft17).unwrap(); // value
		let mut bytes = buf.freeze();
		Properties::decode(&mut bytes, Version::Draft17).unwrap();
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
		Properties::decode(&mut bytes, Version::Draft17).unwrap();
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
		Properties::decode(&mut bytes, Version::Draft17).unwrap();
		assert!(!bytes.has_remaining());
	}

	#[test]
	fn test_round_trip() {
		let properties = Properties {
			max_cache_duration: None,
			timescale: Some(Timescale::MICRO),
			priority: Some(37),
			group_order: Some(GroupOrder::Descending),
		};

		let mut buf = BytesMut::new();
		properties.encode(&mut buf, Version::Draft18).unwrap();

		let mut bytes = buf.freeze();
		assert_eq!(Properties::decode(&mut bytes, Version::Draft18).unwrap(), properties);
		assert!(!bytes.has_remaining());
	}

	#[test]
	fn publisher_priority_is_bounded_to_a_byte() {
		for version in [
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			let mut buf = BytesMut::new();
			0x0eu64.encode(&mut buf, version).unwrap();
			256u64.encode(&mut buf, version).unwrap();
			assert!(matches!(
				Properties::decode(&mut buf.freeze(), version),
				Err(DecodeError::InvalidValue)
			));
		}
	}

	/// Only Ascending and Descending are defined, so 0x0 is a malformed message rather than
	/// the "publisher decides" it means in the draft-14 fields.
	#[test]
	fn test_rejects_zero_group_order() {
		let mut buf = BytesMut::new();
		0x22u64.encode(&mut buf, Version::Draft18).unwrap();
		0u64.encode(&mut buf, Version::Draft18).unwrap();

		let mut bytes = buf.freeze();
		assert!(Properties::decode(&mut bytes, Version::Draft18).is_err());
	}

	/// Draft-16 carries the same block under the name Track Extensions. We don't write one
	/// there, but a peer that does must be understood rather than faulted.
	#[test]
	fn test_decodes_draft16_track_extensions() {
		let mut buf = BytesMut::new();
		0x22u64.encode(&mut buf, Version::Draft16).unwrap();
		2u64.encode(&mut buf, Version::Draft16).unwrap();

		let mut bytes = buf.freeze();
		let properties = Properties::decode(&mut bytes, Version::Draft16).unwrap();
		assert_eq!(properties.group_order, Some(GroupOrder::Descending));
		assert!(!bytes.has_remaining());
	}

	/// The group order property is delta-encoded against the timescale that precedes it,
	/// so it has to survive the timescale being absent.
	#[test]
	fn test_round_trip_group_order_only() {
		let properties = Properties {
			max_cache_duration: None,
			timescale: None,
			priority: None,
			group_order: Some(GroupOrder::Descending),
		};

		let mut buf = BytesMut::new();
		properties.encode(&mut buf, Version::Draft18).unwrap();

		let mut bytes = buf.freeze();
		assert_eq!(Properties::decode(&mut bytes, Version::Draft18).unwrap(), properties);
		assert!(!bytes.has_remaining());
	}
}
