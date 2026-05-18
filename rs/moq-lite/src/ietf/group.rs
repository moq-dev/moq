use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use num_enum::{IntoPrimitive, TryFromPrimitive};

use super::Version;
use crate::ietf::Param;

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
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		u8::from(*self).encode(w, version)?;
		Ok(())
	}
}

impl Decode<Version> for GroupOrder {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Self::try_from(u8::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)
	}
}

impl Param for GroupOrder {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		u8::from(*self).param_encode(w, version)
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let v = u8::param_decode(r, version)?;
		Ok(GroupOrder::try_from(v)
			.unwrap_or(GroupOrder::Descending)
			.any_to_descending())
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
		// Draft-18+: set FIRST_OBJECT. moq-lite always starts subgroups at object 0
		// and never has gaps, so this is unconditionally true on the publisher side.
		if !matches!(
			version,
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17
		) {
			id |= Self::FIRST_OBJECT_BIT;
		}
		Ok(id)
	}

	pub fn decode(id: u64, version: Version) -> Result<Self, DecodeError> {
		// Draft-18+ allows bit 0x40 (FIRST_OBJECT). Strip it before range check;
		// moq-lite already assumes every subgroup starts at object 0, so the bit
		// value carries no extra information for us.
		let id = if matches!(
			version,
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17
		) {
			id
		} else {
			id & !Self::FIRST_OBJECT_BIT
		};

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
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding group header");
		self.flags.encode(version)?.encode(w, version)?;
		self.track_alias.encode(w, version)?;
		self.group_id.encode(w, version)?;

		if !self.flags.has_subgroup && self.sub_group_id != 0 {
			return Err(EncodeError::InvalidState);
		}

		if self.flags.has_subgroup {
			self.sub_group_id.encode(w, version)?;
		}

		// Publisher priority (only if has_priority flag is set)
		if self.flags.has_priority {
			self.publisher_priority.encode(w, version)?;
		}
		Ok(())
	}
}

impl Decode<Version> for GroupHeader {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let flags = GroupFlags::decode(u64::decode(r, version)?, version)?;
		let track_alias = u64::decode(r, version)?;
		let group_id = u64::decode(r, version)?;

		let sub_group_id = match flags.has_subgroup {
			true => u64::decode(r, version)?,
			false => 0,
		};

		// Priority present only if has_priority flag is set
		let publisher_priority = if flags.has_priority {
			u8::decode(r, version)?
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

	/// Regression: a publisher-emitted Draft18 GroupHeader byte must satisfy the
	/// subscriber's uni-stream classifier mask `(byte & 0x90) == 0x10`. Otherwise
	/// the uni stream is dropped as UnexpectedStream and the data plane stalls.
	#[test]
	fn test_draft18_group_header_passes_stream_classifier() {
		let header = GroupHeader {
			track_alias: 1,
			group_id: 0,
			sub_group_id: 0,
			publisher_priority: 0,
			flags: GroupFlags::default(),
		};

		let mut buf = bytes::BytesMut::new();
		header.encode(&mut buf, Version::Draft18).unwrap();
		let type_byte = buf[0] as u64;

		// The check in session.rs::run_uni_group.
		assert_eq!(
			type_byte & 0x90,
			0x10,
			"draft-18 SUBGROUP_HEADER type 0x{type_byte:02x} not recognized by uni-stream classifier",
		);
	}
}
