use std::convert::TryFrom;
use std::fmt;

#[derive(Debug, Copy, Clone)]
#[repr(u32)]
/// Represents how fields are interlaced (if they are)
pub enum FieldOrder {
	/// Progressive, Top, Bottom, or Interlaced is acceptable; driver will pick one
	Any = 0,
	/// progressive, not interlaced
	Progressive = 1,
	/// top, or odd, field
	Top = 2,
	/// bottom, or even, field
	Bottom = 3,
	/// both fields interlaced
	Interlaced = 4,
	/// top field stored first, then bottom field
	SequentialTB = 5,
	/// bottom field stored first, then top field
	SequentialBT = 6,
	/// one field at a time, alternates between top and bottom
	Alternate = 7,
	/// both fields interlaced, starts with top
	InterlacedTB = 8,
	/// both fields interlaced, starts with bottom
	InterlacedBT = 9,
}

impl fmt::Display for FieldOrder {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Any => write!(f, "any"),
			Self::Progressive => write!(f, "progressive"),
			Self::Top => write!(f, "top"),
			Self::Bottom => write!(f, "bottom"),
			Self::Interlaced => write!(f, "interlaced"),
			Self::SequentialTB => write!(f, "sequential, top then bottom"),
			Self::SequentialBT => write!(f, "sequential, bottom then top"),
			Self::Alternate => write!(f, "alternate between fields"),
			Self::InterlacedTB => write!(f, "interlaced, starting with top"),
			Self::InterlacedBT => write!(f, "interlaced, starting with bottom"),
		}
	}
}

impl TryFrom<u32> for FieldOrder {
	type Error = ();

	fn try_from(code: u32) -> Result<Self, Self::Error> {
		match code {
			0 => Ok(Self::Any),
			1 => Ok(Self::Progressive),
			2 => Ok(Self::Top),
			3 => Ok(Self::Bottom),
			4 => Ok(Self::Interlaced),
			5 => Ok(Self::SequentialTB),
			6 => Ok(Self::SequentialBT),
			7 => Ok(Self::Alternate),
			8 => Ok(Self::InterlacedTB),
			9 => Ok(Self::InterlacedBT),
			_ => Err(()),
		}
	}
}
