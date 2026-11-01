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

/// Implementation limit for an object's extension (property) block, independent of the
/// IETF draft. Checked at the length prefix, before the block is buffered.
pub(crate) const MAX_OBJECT_EXTENSIONS: usize = 64 * 1024;

/// The length prefix of an object's extension block, refused past [`MAX_OBJECT_EXTENSIONS`].
#[derive(Debug)]
pub(super) struct ObjectExtensionsLength(pub usize);

impl<'a> Decode<'a, Version> for ObjectExtensionsLength {
	fn decode(buf: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let size = usize::try_from(buf.varint()?).map_err(|_| DecodeError::BoundsExceeded)?;
		if size > MAX_OBJECT_EXTENSIONS {
			return Err(DecodeError::MessageTooLarge {
				size,
				max: MAX_OBJECT_EXTENSIONS,
			});
		}
		Ok(Self(size))
	}
}

/// Encode a frame's presentation timestamp as a moq-transport Object Property.
///
/// Matches the LOC encoding of the same registry id so a relay or LOC-aware peer
/// reads the same bytes. The Timestamp value is always absolute. Writes the raw KVP bytes
/// (no outer length prefix); the caller frames the block with its byte length.
///
/// The units come from the track's TIMESCALE property, not from here: repeating the
/// timescale on every object would cost bytes per frame to restate something fixed for
/// the track's lifetime. `timescale` is what the track advertised, and the timestamp is
/// converted into it so the value on the wire matches the declared units.
pub fn encode_object_time(w: &mut Encoder<'_>, timestamp: Timestamp, timescale: Timescale) -> Result<(), EncodeError> {
	let timestamp = timestamp.convert(timescale).map_err(|_| EncodeError::BoundsExceeded)?;
	// The first property's type is a delta from zero, so it is written as is.
	w.varint(PROP_TIMESTAMP)?;
	w.varint(timestamp.value())?;
	Ok(())
}

/// Decode the same property list as ingest when checking timestamps in wire tests.
#[cfg(test)]
pub fn decode_object_time(
	r: &mut Decoder<'_>,
	timescale: Timescale,
	version: Version,
) -> Result<Option<Timestamp>, DecodeError> {
	let data = bytes::Bytes::copy_from_slice(r.rest());
	super::object::read(&data, version, Some(timescale))
		.map(|decoded| decoded.timestamp)
		.map_err(|error| match error {
			crate::Error::Decode(error) => error,
			_ => DecodeError::InvalidValue,
		})
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

	/// The model's order: `Any` leaves the choice to the publisher, which serves newest
	/// first.
	pub fn to_order(self) -> crate::track::Order {
		match self {
			Self::Ascending => crate::track::Order::Ascending,
			Self::Any | Self::Descending => crate::track::Order::Descending,
		}
	}
}

impl From<crate::track::Order> for GroupOrder {
	fn from(order: crate::track::Order) -> Self {
		match order {
			crate::track::Order::Ascending => Self::Ascending,
			crate::track::Order::Descending => Self::Descending,
		}
	}
}

impl Encode<Version> for GroupOrder {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.u8(u8::from(*self));
		Ok(())
	}
}

impl<'a> Decode<'a, Version> for GroupOrder {
	fn decode(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		Self::try_from(r.u8()?).map_err(|_| DecodeError::InvalidValue)
	}
}

impl Param for GroupOrder {
	fn param_encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		u8::from(*self).param_encode(w, version)
	}

	fn param_decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match u8::param_decode(r, version)? {
			1 => Ok(GroupOrder::Ascending),
			2 => Ok(GroupOrder::Descending),
			_ => Err(DecodeError::InvalidValue),
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupFlags {
	// The group has extensions.
	pub has_extensions: bool,

	// There's an explicit subgroup on the wire.
	pub has_subgroup: bool,

	// Use the first object ID as the subgroup ID, so only a first object ID of 0 is
	// subgroup 0. Not compatible with has_subgroup.
	pub has_subgroup_object: bool,

	// There's an implicit end marker when the stream is closed.
	pub has_end: bool,

	// v15: whether priority is present in the header.
	// When false (0x30 base), priority inherits from the control message.
	pub has_priority: bool,

	/// Whether this stream carries the subgroup from its first published object.
	///
	/// Independent of its first Object ID: a publisher can start a subgroup above 0.
	/// Always true before draft-18, where there is no such signal to read.
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

	pub fn encode(&self) -> Result<u64, EncodeError> {
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
		if self.first_object {
			id |= Self::FIRST_OBJECT_BIT;
		}
		Ok(id)
	}

	pub fn decode(id: u64) -> Result<Self, DecodeError> {
		// Bit 0x40 (FIRST_OBJECT) says the stream carries the subgroup from its first
		// published object. Strip it before the range check, but keep the value: it is the
		// only signal that a subgroup starts partway through.
		let first_object = (id & Self::FIRST_OBJECT_BIT) != 0;
		let id = id & !Self::FIRST_OBJECT_BIT;

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
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding group header");
		w.varint(self.flags.encode()?)?;
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

impl<'a> Decode<'a, Version> for GroupHeader {
	fn decode(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let flags = GroupFlags::decode(r.varint()?)?;
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
			super::priority::DEFAULT_WIRE
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
		encode_object_time(&mut Encoder::new(&mut buf, version.into()), ts, timescale).unwrap();
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

	/// No Timestamp property at all yields None.
	#[test]
	fn test_object_time_absent() {
		assert!(decode_time(&[], Timescale::MICRO, Version::Draft18).is_none());
	}

	// Test table from draft-ietf-moq-transport-14 Section 10.4.2 Table 7
	#[test]
	fn test_group_flags_spec_table() {
		// Type 0x10: No subgroup field, Subgroup ID = 0, No extensions, No end
		let flags = GroupFlags::decode(0x10).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert!(flags.has_priority);
		assert_eq!(flags.encode().unwrap(), 0x10);

		// Type 0x11: No subgroup field, Subgroup ID = 0, Extensions, No end
		let flags = GroupFlags::decode(0x11).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x11);

		// Type 0x12: No subgroup field, Subgroup ID = First Object ID, No extensions, No end
		let flags = GroupFlags::decode(0x12).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x12);

		// Type 0x13: No subgroup field, Subgroup ID = First Object ID, Extensions, No end
		let flags = GroupFlags::decode(0x13).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x13);

		// Type 0x14: Subgroup field present, No extensions, No end
		let flags = GroupFlags::decode(0x14).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x14);

		// Type 0x15: Subgroup field present, Extensions, No end
		let flags = GroupFlags::decode(0x15).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x15);

		// Type 0x18: No subgroup field, Subgroup ID = 0, No extensions, End of group
		let flags = GroupFlags::decode(0x18).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x18);

		// Type 0x19: No subgroup field, Subgroup ID = 0, Extensions, End of group
		let flags = GroupFlags::decode(0x19).unwrap();
		assert!(!flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x19);

		// Type 0x1A: No subgroup field, Subgroup ID = First Object ID, No extensions, End of group
		let flags = GroupFlags::decode(0x1A).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x1A);

		// Type 0x1B: No subgroup field, Subgroup ID = First Object ID, Extensions, End of group
		let flags = GroupFlags::decode(0x1B).unwrap();
		assert!(!flags.has_subgroup);
		assert!(flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x1B);

		// Type 0x1C: Subgroup field present, No extensions, End of group
		let flags = GroupFlags::decode(0x1C).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(!flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x1C);

		// Type 0x1D: Subgroup field present, Extensions, End of group
		let flags = GroupFlags::decode(0x1D).unwrap();
		assert!(flags.has_subgroup);
		assert!(!flags.has_subgroup_object);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x1D);

		// Invalid: Both has_subgroup and has_subgroup_object (would be 0x16)
		assert!(GroupFlags::decode(0x16).is_err());
	}

	#[test]
	fn test_group_flags_no_priority_range() {
		// v15: 0x30 range = same flags as 0x10 range but no priority
		let flags = GroupFlags::decode(0x30).unwrap();
		assert!(!flags.has_priority);
		assert!(!flags.has_subgroup);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x30);

		let flags = GroupFlags::decode(0x38).unwrap();
		assert!(!flags.has_priority);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x38);

		let flags = GroupFlags::decode(0x3D).unwrap();
		assert!(!flags.has_priority);
		assert!(flags.has_subgroup);
		assert!(flags.has_extensions);
		assert!(flags.has_end);
		assert_eq!(flags.encode().unwrap(), 0x3D);

		// Invalid: Both has_subgroup and has_subgroup_object in no-priority range
		assert!(GroupFlags::decode(0x36).is_err());
	}

	/// FIRST_OBJECT says the stream carries the subgroup from its first published object.
	/// It is the only signal that a group arrived with its head missing, so the value has
	/// to survive decode rather than being stripped with the bit.
	#[test]
	fn first_object_round_trips() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for first_object in [true, false] {
				let flags = GroupFlags {
					first_object,
					..Default::default()
				};
				let encoded = flags.encode().unwrap();
				assert_eq!(
					(encoded & GroupFlags::FIRST_OBJECT_BIT) != 0,
					first_object,
					"{version} {first_object}"
				);
				assert_eq!(GroupFlags::decode(encoded).unwrap().first_object, first_object);
			}
		}
	}

	/// Draft-18 byte 0x70..=0x7D should decode to the same flags as 0x30..=0x3D.
	#[test]
	fn test_draft18_extended_range() {
		// 0x70 = 0x30 (no-priority, no flags) + 0x40 (FIRST_OBJECT)
		let flags = GroupFlags::decode(0x70).unwrap();
		assert!(!flags.has_priority);
		assert!(!flags.has_subgroup);
		assert!(!flags.has_extensions);
		assert!(!flags.has_end);

		// 0x7D = 0x3D + 0x40
		let flags = GroupFlags::decode(0x7D).unwrap();
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
			GroupFlags::decode(type_byte).is_ok(),
			"draft-18 SUBGROUP_HEADER type 0x{type_byte:02x} not recognized by uni-stream classifier",
		);
	}
}
