use bitflags::bitflags;
use std::{fmt, str};

use crate::sys::*;

bitflags! {
	#[allow(clippy::unreadable_literal)]
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub struct Flags: u32 {
		const VIDEO_CAPTURE         = 0x00000001;
		const VIDEO_OUTPUT          = 0x00000002;
		const VIDEO_OVERLAY         = 0x00000004;
		const VBI_CAPTURE           = 0x00000010;
		const VBI_OUTPUT            = 0x00000020;
		const SLICED_VBI_CAPTURE    = 0x00000040;
		const SLICED_VBI_OUTPUT     = 0x00000080;
		const RDS_CAPTURE           = 0x00000100;
		const VIDEO_OUTPUT_OVERLAY  = 0x00000200;
		const HW_FREQ_SEEK          = 0x00000400;
		const RDS_OUTPUT            = 0x00000800;

		const VIDEO_CAPTURE_MPLANE  = 0x00001000;
		const VIDEO_OUTPUT_MPLANE   = 0x00002000;
		const VIDEO_M2M_MPLANE      = 0x00004000;
		const VIDEO_M2M             = 0x00008000;

		const TUNER                 = 0x00010000;
		const AUDIO                 = 0x00020000;
		const RADIO                 = 0x00040000;
		const MODULATOR             = 0x00080000;

		const SDR_CAPTURE           = 0x00100000;
		const EXT_PIX_FORMAT        = 0x00200000;
		const SDR_OUTPUT            = 0x00400000;
		const META_CAPTURE          = 0x00800000;

		const READ_WRITE            = 0x01000000;
		const ASYNC_IO              = 0x02000000;
		const STREAMING             = 0x04000000;
		const META_OUTPUT           = 0x08000000;

		const TOUCH                 = 0x10000000;

		const DEVICE_CAPS           = 0x80000000;
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
/// Device capabilities
pub struct Capabilities {
	/// Driver name, e.g. uvc for usb video class devices
	pub driver: String,
	/// Card name
	pub card: String,
	/// Bus name, e.g. USB or PCI
	pub bus: String,
	/// Version number MAJOR.MINOR.PATCH
	pub version: (u8, u8, u8),

	/// Capability flags
	pub capabilities: Flags,
}

impl From<v4l2_capability> for Capabilities {
	fn from(cap: v4l2_capability) -> Self {
		Self {
			driver: str::from_utf8(&cap.driver)
				.unwrap()
				.trim_matches(char::from(0))
				.to_string(),
			card: str::from_utf8(&cap.card)
				.unwrap()
				.trim_matches(char::from(0))
				.to_string(),
			bus: str::from_utf8(&cap.bus_info)
				.unwrap()
				.trim_matches(char::from(0))
				.to_string(),
			version: (
				((cap.version >> 16) & 0xff) as u8,
				((cap.version >> 8) & 0xff) as u8,
				(cap.version & 0xff) as u8,
			),
			capabilities: Flags::from(cap.device_caps),
		}
	}
}

impl fmt::Display for Capabilities {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "Driver      : {}", self.driver)?;
		writeln!(f, "Card        : {}", self.card)?;
		writeln!(f, "Bus         : {}", self.bus)?;
		writeln!(
			f,
			"Version     : {}.{}.{}",
			self.version.0, self.version.1, self.version.2
		)?;
		writeln!(f, "Capabilities : {}", self.capabilities)?;
		Ok(())
	}
}
