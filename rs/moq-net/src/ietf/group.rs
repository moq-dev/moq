use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};
use crate::{Timescale, Timestamp};

use num_enum::{IntoPrimitive, TryFromPrimitive};

use super::Version;
use crate::ietf::Param;

/// MOQ Object Property IDs (the MOQ Object Properties registry, shared with
/// draft-ietf-moq-loc-04). Even type ids carry a single varint value. The object-scope
/// Timescale is only skipped on decode, so only tests name it.
#[cfg(test)]
const PROP_TIMESCALE: u64 = 0x08;
const PROP_TIMESTAMP: u64 = 0x10;

/// The Timestamp id from draft-ietf-moq-loc-03, accepted on decode only.
///
/// Draft-03's body text and its IANA table disagreed (0x0A vs 0x06); this is the
/// table's value, which is what we and other implementations shipped. Draft-04
/// assigns 0x0A to Secure Objects private properties, so it is not accepted here.
const PROP_TIMESTAMP_DRAFT03: u64 = 0x06;

/// Encode a frame's presentation timestamp as a moq-transport Object Property.
///
/// Matches the LOC encoding of the same registry id so a relay or LOC-aware peer
/// reads the same bytes on drafts that delta-encode KVP type ids. The Timestamp
/// value is always absolute. Writes the raw KVP bytes
/// (no outer length prefix); the caller frames the block with its byte length.
///
/// The units come from the track's TIMESCALE property, not from here: repeating the
/// timescale on every object would cost bytes per frame to restate something fixed for
/// the track's lifetime. `timescale` is what the track advertised, and the timestamp is
/// converted into it so the value on the wire matches the declared units.
pub fn encode_object_time(
	w: &mut Encoder<'_>,
	timestamp: Timestamp,
	timescale: Timescale,
	version: Version,
) -> Result<(), EncodeError> {
	let timestamp = timestamp.convert(timescale).map_err(|_| EncodeError::BoundsExceeded)?;
	encode_object_property_type(w, PROP_TIMESTAMP, 0, version)?;
	w.varint(timestamp.value())?;
	Ok(())
}

fn encode_object_property_type(w: &mut Encoder<'_>, kind: u64, prev: u64, version: Version) -> Result<(), EncodeError> {
	let encoded = match version {
		Version::Draft14 | Version::Draft15 => kind,
		_ => kind.checked_sub(prev).ok_or(EncodeError::BoundsExceeded)?,
	};
	w.varint(encoded)
}

/// Decode the Timestamp (0x10) Object Property from an object's extension block,
/// skipping any other properties. Returns `None` when no Timestamp property is present.
///
/// `timescale` is the track's declared units. An object-scope Timescale (0x08), which
/// draft-ietf-moq-loc-04 permits, is ignored: timedness and units are per track, so the
/// track's TIMESCALE alone decides them.
pub fn decode_object_time(
	r: &mut Decoder<'_>,
	timescale: Timescale,
	version: Version,
) -> Result<Option<Timestamp>, DecodeError> {
	let mut timestamp: Option<u64> = None;
	let mut prev_type: u64 = 0;
	let mut first = true;

	while !r.is_empty() {
		let step = r.varint()?;
		let abs = match version {
			Version::Draft14 | Version::Draft15 => step,
			_ if first => step,
			_ => prev_type.checked_add(step).ok_or(DecodeError::BoundsExceeded)?,
		};
		first = false;
		prev_type = abs;

		if abs % 2 == 0 {
			// Even type: a single varint value.
			let value = r.varint()?;
			match abs {
				PROP_TIMESTAMP | PROP_TIMESTAMP_DRAFT03 => timestamp = Some(value),
				_ => {}
			}
		} else {
			// Odd type: length-prefixed bytes we don't care about.
			let len = usize::try_from(r.varint()?).map_err(|_| DecodeError::BoundsExceeded)?;
			r.slice(len)?;
		}
	}

	timestamp
		.map(|value| Timestamp::new(value, timescale).map_err(|_| DecodeError::InvalidValue))
		.transpose()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum GroupOrder {
	Any = 0x0,
	Ascending = 0x1,
	Descending = 0x2,
}

impl GroupOrder {
	/// Map `Any` (0x0) to `Descending`, leaving other values unchanged.
	pub fn any_to_descending(self) -> Self {
		match self {
			Self::Any => Self::Descending,
			other => other,
		}
	}
}

impl Encode<Version> for GroupOrder {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.u8(u8::from(*self));
		Ok(())
	}
}

impl Decode<Version> for GroupOrder {
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		Self::try_from(r.u8()?).map_err(|_| DecodeError::InvalidValue)
	}
}

impl Param for GroupOrder {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		u8::from(*self).param_encode(w, version)
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let v = u8::param_decode(r, version)?;
		match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => Ok(GroupOrder::try_from(v)
				.unwrap_or(GroupOrder::Descending)
				.any_to_descending()),
			_ => match v {
				1 => Ok(GroupOrder::Ascending),
				2 => Ok(GroupOrder::Descending),
				_ => Err(DecodeError::InvalidValue),
			},
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupFlags {
	// The group has extensions.
	pub has_extensions: bool,

	// There's an explicit subgroup on the wire.
	pub has_subgroup: bool,

	// Use the first object ID as the subgroup ID
	// Since we don't support subgroups or object ID > 0, this is trivial to support.
	// Not compatibile with has_subgroup
	pub has_subgroup_object: bool,

	// There's an implicit end marker when the stream is closed.
	pub has_end: bool,

	// v15: whether priority is present in the header.
	// When false (0x30 base), priority inherits from the control message.
	pub has_priority: bool,

	/// Whether this stream carries the subgroup from its first published object.
	///
	/// A subgroup that starts partway through has a hole at the front, which moq-lite cannot
	/// represent. Always true on the drafts that predate the FIRST_OBJECT bit (before
	/// draft-18), where there is no such signal to read.
	pub first_object: bool,
}

impl GroupFlags {
	// v14 range: 0x10-0x1d (priority always present)
	pub const START: u64 = 0x10;
	pub const END: u64 = 0x1d;

	// v15 adds: 0x30-0x3d (priority absent, inherits from control message)
	pub const START_NO_PRIORITY: u64 = 0x30;
	pub const END_NO_PRIORITY: u64 = 0x3d;

	// draft-18 adds bit 0x40 (FIRST_OBJECT) per spec §11.4.2.
	// moq-lite always sets this bit on emit because every subgroup starts at object 0.
	pub const FIRST_OBJECT_BIT: u64 = 0x40;

	pub fn encode(&self, version: Version) -> Result<u64, EncodeError> {
		if self.has_subgroup && self.has_subgroup_object {
			return Err(EncodeError::InvalidState);
		}

		let base = if self.has_priority {
			Self::START
		} else {
			Self::START_NO_PRIORITY
		};
		let mut id: u64 = base;
		if self.has_extensions {
			id |= 0x01;
		}
		if self.has_subgroup_object {
			id |= 0x02;
		}
		if self.has_subgroup {
			id |= 0x04;
		}
		if self.has_end {
			id |= 0x08;
		}
		// Draft-18+: moq-lite always starts subgroups at object 0 and never has gaps, so
		// the publisher side sets this on everything it produces.
		if self.first_object
			&& !matches!(
				version,
				Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17
			) {
			id |= Self::FIRST_OBJECT_BIT;
		}
		Ok(id)
	}

	pub fn decode(id: u64, version: Version) -> Result<Self, DecodeError> {
		// Draft-18+ allows bit 0x40 (FIRST_OBJECT), which says the stream carries the
		// subgroup from its first published object. Strip it before the range check, but
		// keep the value: it is the only signal that a subgroup starts partway through.
		// The drafts that predate it carry no such signal, so they are taken at their word.
		let legacy = matches!(
			version,
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17
		);
		let first_object = legacy || (id & Self::FIRST_OBJECT_BIT) != 0;
		let id = if legacy { id } else { id & !Self::FIRST_OBJECT_BIT };

		let (has_priority, base_id) = if (Self::START..=Self::END).contains(&id) {
			(true, id)
		} else if (Self::START_NO_PRIORITY..=Self::END_NO_PRIORITY).contains(&id) {
			(false, id - (Self::START_NO_PRIORITY - Self::START))
		} else {
			return Err(DecodeError::InvalidValue);
		};

		let has_extensions = (base_id & 0x01) != 0;
		let has_subgroup_object = (base_id & 0x02) != 0;
		let has_subgroup = (base_id & 0x04) != 0;
		let has_end = (base_id & 0x08) != 0;

		if has_subgroup && has_subgroup_object {
			return Err(DecodeError::InvalidValue);
		}

		Ok(Self {
			first_object,
			has_extensions,
			has_subgroup,
			has_subgroup_object,
			has_end,
			has_priority,
		})
	}
}

impl Default for GroupFlags {
	fn default() -> Self {
		Self {
			has_extensions: false,
			has_subgroup: false,
			has_subgroup_object: false,
			has_end: true,
			has_priority: true,
			first_object: true,
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupHeader {
	pub track_alias: u64,
	pub group_id: u64,
	pub sub_group_id: u64,
	pub publisher_priority: u8,
	pub flags: GroupFlags,
}

impl Encode<Version> for GroupHeader {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding group header");
		w.varint(self.flags.encode(version)?)?;
		w.varint(self.track_alias)?;
		w.varint(self.group_id)?;

		if !self.flags.has_subgroup && self.sub_group_id != 0 {
			return Err(EncodeError::InvalidState);
		}

		if self.flags.has_subgroup {
			w.varint(self.sub_group_id)?;
		}

		// Publisher priority (only if has_priority flag is set)
		if self.flags.has_priority {
			w.u8(self.publisher_priority);
		}
		Ok(())
	}
}

impl Decode<Version> for GroupHeader {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let flags = GroupFlags::decode(r.varint()?, version)?;
		let track_alias = r.varint()?;
		let group_id = r.varint()?;

		let sub_group_id = match flags.has_subgroup {
			true => r.varint()?,
			false => 0,
		};

		// Priority present only if has_priority flag is set
		let publisher_priority = if flags.has_priority {
			r.u8()?
		} else {
			128 // Default priority when absent
		};

		let result = Self {
			track_alias,
			group_id,
			sub_group_id,
			publisher_priority,
			flags,
		};
		tracing::trace!(?result, "decoded group header");
		Ok(result)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Encode an object's properties at `version`.
	fn encode_time(ts: Timestamp, timescale: Timescale, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		encode_object_time(&mut Encoder::new(&mut buf, version.into()), ts, timescale, version).unwrap();
		buf
	}

	/// Read `buf` back as a flat list of varints.
	fn varints(buf: &[u8], version: Version) -> Vec<u64> {
		let mut r = Decoder::new(buf, version.into());
		std::iter::from_fn(|| (!r.is_empty()).then(|| r.varint().unwrap())).collect()
	}

	/// Write `values` as a flat list of varints.
	fn from_varints(values: &[u64], version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, version.into());
		for value in values {
			w.varint(*value).unwrap();
		}
		buf
	}

	fn decode_time(buf: &[u8], timescale: Timescale, version: Version) -> Option<Timestamp> {
		let mut r = Decoder::new(buf, version.into());
		let decoded = decode_object_time(&mut r, timescale, version).unwrap();
		assert!(r.is_empty());
		decoded
	}

	/// An object Timestamp round-trips through encode/decode at the track's scale.
	#[test]
	fn test_object_time_roundtrip() {
		let ts = Timestamp::new(96_000, Timescale::MICRO).unwrap();
		let buf = encode_time(ts, Timescale::MICRO, Version::Draft18);

		let decoded = decode_time(&buf, Timescale::MICRO, Version::Draft18).unwrap();
		assert_eq!(decoded.value(), 96_000);
		assert_eq!(decoded.scale(), Timescale::MICRO);
	}

	/// The value on the wire is in the track's units, not the frame's.
	#[test]
	fn test_object_time_converts_into_the_track_scale() {
		// 2 seconds, expressed in milliseconds by the frame.
		let ts = Timestamp::new(2_000, Timescale::MILLI).unwrap();
		let buf = encode_time(ts, Timescale::MICRO, Version::Draft18);
		assert_eq!(varints(&buf, Version::Draft18), [PROP_TIMESTAMP, 2_000_000]);

		let decoded = decode_time(&buf, Timescale::MICRO, Version::Draft18).unwrap();
		assert_eq!(decoded.value(), 2_000_000);
		assert_eq!(decoded.scale(), Timescale::MICRO);
	}

	/// No Timescale rides along with the object; the track declared it once.
	#[test]
	fn test_object_time_omits_the_timescale() {
		let ts = Timestamp::new(96_000, Timescale::MILLI).unwrap();
		let buf = encode_time(ts, Timescale::MILLI, Version::Draft16);
		assert_eq!(varints(&buf, Version::Draft16), [PROP_TIMESTAMP, 96_000]);
	}

	/// Draft-14/15 write absolute property types rather than deltas.
	#[test]
	fn test_object_time_legacy_uses_absolute_types() {
		let ts = Timestamp::new(96_000, Timescale::MILLI).unwrap();
		let buf = encode_time(ts, Timescale::MILLI, Version::Draft15);
		assert_eq!(varints(&buf, Version::Draft15), [PROP_TIMESTAMP, 96_000]);
	}

	/// An object-scope Timescale (which LOC permits) is ignored: the track's units apply.
	#[test]
	fn test_object_time_ignores_an_object_scope_timescale() {
		let buf = from_varints(
			&[
				PROP_TIMESCALE,
				u64::from(Timescale::MILLI),
				PROP_TIMESTAMP - PROP_TIMESCALE,
				42,
			],
			Version::Draft18,
		);

		let decoded = decode_time(&buf, Timescale::MICRO, Version::Draft18).unwrap();
		assert_eq!(decoded.value(), 42);
		assert_eq!(decoded.scale(), Timescale::MICRO);
	}

	/// The track's timescale supplies the units.
	#[test]
	fn test_object_time_defaults_to_the_track_scale() {
		let buf = from_varints(&[PROP_TIMESTAMP, 1234], Version::Draft18);

		let decoded = decode_time(&buf, Timescale::MILLI, Version::Draft18).unwrap();
		assert_eq!(decoded.value(), 1234);
		assert_eq!(decoded.scale(), Timescale::MILLI);
	}

	/// A peer on draft-ietf-moq-loc-03 wrote the Timestamp at 0x06; still decode it.
	#[test]
	fn test_object_time_decodes_draft03_timestamp() {
		let buf = from_varints(&[PROP_TIMESTAMP_DRAFT03, 777], Version::Draft18);

		let decoded = decode_time(&buf, Timescale::MICRO, Version::Draft18).unwrap();
		assert_eq!(decoded.value(), 777);
		assert_eq!(decoded.scale(), Timescale::MICRO);
	}

	/// No Timestamp property at all yields None.
	#[test]
	fn test_object_time_absent() {
		assert!(decode_time(&[], Timescale::MICRO, Version::Draft18).is_none());
	}

	// Test table from draft-ietf-moq-transport-14 Section 10.4.2 Table 7
	#[test]
	fn test_group_flags_spec_table() {
		// Type 0x10: No subgroup field, Subgroup ID = 0, No extensions, No end
		let flags = GroupFlags::decode(0x10, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert!(flags.has_priority);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x10);

		// Type 0x11: No subgroup field, Subgroup ID = 0, Extensions, No end
		let flags = GroupFlags::decode(0x11, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x11);

		// Type 0x12: No subgroup field, Subgroup ID = First Object ID, No extensions, No end
		let flags = GroupFlags::decode(0x12, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x12);

		// Type 0x13: No subgroup field, Subgroup ID = First Object ID, Extensions, No end
		let flags = GroupFlags::decode(0x13, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x13);

		// Type 0x14: Subgroup field present, No extensions, No end
		let flags = GroupFlags::decode(0x14, Version::Draft14).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x14);

		// Type 0x15: Subgroup field present, Extensions, No end
		let flags = GroupFlags::decode(0x15, Version::Draft14).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x15);

		// Type 0x18: No subgroup field, Subgroup ID = 0, No extensions, End of group
		let flags = GroupFlags::decode(0x18, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x18);

		// Type 0x19: No subgroup field, Subgroup ID = 0, Extensions, End of group
		let flags = GroupFlags::decode(0x19, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x19);

		// Type 0x1A: No subgroup field, Subgroup ID = First Object ID, No extensions, End of group
		let flags = GroupFlags::decode(0x1A, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x1A);

		// Type 0x1B: No subgroup field, Subgroup ID = First Object ID, Extensions, End of group
		let flags = GroupFlags::decode(0x1B, Version::Draft14).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x1B);

		// Type 0x1C: Subgroup field present, No extensions, End of group
		let flags = GroupFlags::decode(0x1C, Version::Draft14).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x1C);

		// Type 0x1D: Subgroup field present, Extensions, End of group
		let flags = GroupFlags::decode(0x1D, Version::Draft14).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x1D);

		// Invalid: Both has_subgroup and has_subgroup_object (would be 0x16)
		assert!(GroupFlags::decode(0x16, Version::Draft14).is_err());
	}

	#[test]
	fn test_group_flags_no_priority_range() {
		// v15: 0x30 range = same flags as 0x10 range but no priority
		let flags = GroupFlags::decode(0x30, Version::Draft14).unwrap();
		assert!(!flags.has_priority);
		assert!(!flags.has_subgroup);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x30);

		let flags = GroupFlags::decode(0x38, Version::Draft14).unwrap();
		assert!(!flags.has_priority);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x38);

		let flags = GroupFlags::decode(0x3D, Version::Draft14).unwrap();
		assert!(!flags.has_priority);
		assert!(flags.has_subgroup);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode(Version::Draft14).unwrap(), 0x3D);

		// Invalid: Both has_subgroup and has_subgroup_object in no-priority range
		assert!(GroupFlags::decode(0x36, Version::Draft14).is_err());
	}

	/// Draft-18 introduces the FIRST_OBJECT bit (0x40) per spec §11.4.2.
	/// moq-lite always sets it on emit and ignores it on decode (we already
	/// require what the bit asserts).
	#[test]
	fn test_first_object_bit_draft18() {
		// Encoding sets bit 0x40 for default flags.
		let flags = GroupFlags::default();
		let encoded = flags.encode(Version::Draft18).unwrap();
		assert_eq!(encoded & GroupFlags::FIRST_OBJECT_BIT, GroupFlags::FIRST_OBJECT_BIT);
		// The base value is what draft-17 would have produced.
		let v17 = flags.encode(Version::Draft17).unwrap();
		assert_eq!(encoded, v17 | GroupFlags::FIRST_OBJECT_BIT);

		// Decoding accepts and discards the bit.
		let decoded = GroupFlags::decode(v17 | GroupFlags::FIRST_OBJECT_BIT, Version::Draft18).unwrap();
		assert_eq!(decoded, flags);

		// Draft-17 rejects the FIRST_OBJECT bit (it's outside the 0x10-0x1d / 0x30-0x3d ranges).
		assert!(GroupFlags::decode(v17 | GroupFlags::FIRST_OBJECT_BIT, Version::Draft17).is_err());
	}

	/// FIRST_OBJECT says the stream carries the subgroup from its first published object.
	/// It is the only signal that a group arrived with its head missing, so the value has
	/// to survive decode rather than being stripped with the bit.
	#[test]
	fn first_object_round_trips() {
		for version in [Version::Draft18, Version::Draft19, Version::Draft20] {
			for first_object in [true, false] {
				let flags = GroupFlags {
					first_object,
					..Default::default()
				};
				let encoded = flags.encode(version).unwrap();
				assert_eq!(
					(encoded & GroupFlags::FIRST_OBJECT_BIT) != 0,
					first_object,
					"{version} {first_object}"
				);
				assert_eq!(GroupFlags::decode(encoded, version).unwrap().first_object, first_object);
			}
		}
	}

	/// The bit arrived in draft-18. Earlier drafts carry no such signal, so a group there
	/// is taken at its word rather than read as starting partway through.
	#[test]
	fn older_drafts_have_no_first_object_signal() {
		for version in [Version::Draft14, Version::Draft15, Version::Draft16, Version::Draft17] {
			let flags = GroupFlags {
				first_object: false,
				..Default::default()
			};
			let encoded = flags.encode(version).unwrap();
			assert_eq!(encoded & GroupFlags::FIRST_OBJECT_BIT, 0, "{version}");
			assert!(GroupFlags::decode(encoded, version).unwrap().first_object, "{version}");
		}
	}

	/// Draft-19 makes no changes to the subgroup header wire format, so the flags
	/// byte must be byte-identical to draft-18 on both encode and decode.
	#[test]
	fn test_draft19_matches_draft18() {
		for flags in [
			GroupFlags::default(),
			GroupFlags {
				has_subgroup: true,
				has_extensions: true,
				has_end: true,
				has_subgroup_object: false,
				has_priority: false,
				first_object: true,
			},
		] {
			let v18 = flags.encode(Version::Draft18).unwrap();
			let v19 = flags.encode(Version::Draft19).unwrap();
			assert_eq!(v18, v19, "draft-19 must encode the subgroup header like draft-18");
			assert_eq!(GroupFlags::decode(v19, Version::Draft19).unwrap(), flags);
		}
	}

	/// Draft-18 byte 0x70..=0x7D should decode to the same flags as 0x30..=0x3D.
	#[test]
	fn test_draft18_extended_range() {
		// 0x70 = 0x30 (no-priority, no flags) + 0x40 (FIRST_OBJECT)
		let flags = GroupFlags::decode(0x70, Version::Draft18).unwrap();
		assert!(!flags.has_priority);
		assert!(!flags.has_subgroup);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);

		// 0x7D = 0x3D + 0x40
		let flags = GroupFlags::decode(0x7D, Version::Draft18).unwrap();
		assert!(!flags.has_priority);
		assert!(flags.has_subgroup);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
	}

	/// Regression: a publisher-emitted Draft18 GroupHeader byte must match the
	/// SUBGROUP_HEADER form `(byte & 0x90) == 0x10` and decode as flags, which is how
	/// the uni-stream classifier recognizes it. Otherwise the uni stream is an unknown
	/// type, which closes the session.
	#[test]
	fn test_draft18_group_header_passes_stream_classifier() {
		let header = GroupHeader {
			track_alias: 1,
			group_id: 0,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: GroupFlags::default(),
		};

		let mut buf = Vec::new();
		header
			.encode(&mut Encoder::new(&mut buf, Version::Draft18.into()), Version::Draft18)
			.unwrap();
		let type_byte = buf[0] as u64;

		assert_eq!(
			type_byte & 0x90,
			0x10,
			"draft-18 SUBGROUP_HEADER type 0x{type_byte:02x} is outside the SUBGROUP_HEADER form",
		);
		// The check in session.rs::UniType::classify.
		assert!(
			GroupFlags::decode(type_byte, Version::Draft18).is_ok(),
			"draft-18 SUBGROUP_HEADER type 0x{type_byte:02x} not recognized by uni-stream classifier",
		);
	}
}
