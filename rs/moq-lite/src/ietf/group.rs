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
		Ok(id)
	}

	pub fn decode(id: u64) -> Result<Self, DecodeError> {
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
		self.flags.encode()?.encode(w, version)?;
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
		let flags = GroupFlags::decode(u64::decode(r, version)?)?;
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

		Ok(Self {
			track_alias,
			group_id,
			sub_group_id,
			publisher_priority,
			flags,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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
}
