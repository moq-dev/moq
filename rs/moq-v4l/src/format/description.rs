use bitflags::bitflags;
use std::{fmt, str};

use crate::format::FourCC;
use crate::sys::*;

bitflags! {
	#[allow(clippy::unreadable_literal)]
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub struct Flags : u32 {
		const COMPRESSED            = 0x0001;
		const EMULATED              = 0x0002;
		const CONTINUOUS_BITSTREAM  = 0x0004;
		const DYN_RESOLUTION        = 0x0008;
	}
}

impl From<u32> for Flags {
	fn from(flags: u32) -> Self {
		Self::from_bits_truncate(flags)
	}
}

impl From<Flags> for u32 {
	fn from(flags: Flags) -> Self {
		flags.bits()
	}
}

impl fmt::Display for Flags {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		fmt::Debug::fmt(self, f)
	}
}

#[derive(Debug)]
/// Format description as returned by [`crate::v4l2::vidioc::VIDIOC_ENUM_FMT`]
pub struct Description {
	pub index: u32,
	pub typ: u32,
	pub flags: Flags,
	pub description: String,
	pub fourcc: FourCC,
}

impl fmt::Display for Description {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "index       : {}", self.index)?;
		writeln!(f, "type:       : {}", self.typ)?;
		writeln!(f, "flags:      : {}", self.flags)?;
		writeln!(f, "description : {}", self.description)?;
		writeln!(f, "fourcc      : {}", self.fourcc)?;
		Ok(())
	}
}

impl From<v4l2_fmtdesc> for Description {
	fn from(desc: v4l2_fmtdesc) -> Self {
		Self {
			index: desc.index,
			typ: desc.type_,
			flags: Flags::from(desc.flags),
			description: str::from_utf8(&desc.description)
				.unwrap()
				.trim_matches(char::from(0))
				.to_string(),
			fourcc: FourCC::from(desc.pixelformat),
		}
	}
}
