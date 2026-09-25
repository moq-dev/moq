use std::convert::TryFrom;
use std::fmt;

#[derive(Debug, Copy, Clone)]
#[repr(u32)]
/// Quantization for the colorspace.
///
/// The driver decides this for capture streams and the user sets
/// it for output streams.
pub enum Quantization {
	/// default for the colorspace
	Default = 0,
	/// maps to the full range; 0 goes to 0 and 1 goes to 255
	FullRange = 1,
	/// maps to a limited range; 0 goes to 16 and 1 goes to 235
	LimitedRange = 2,
}

impl fmt::Display for Quantization {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Default => write!(f, "default"),
			Self::FullRange => write!(f, "full range"),
			Self::LimitedRange => write!(f, "limited range"),
		}
	}
}

impl TryFrom<u32> for Quantization {
	type Error = ();

	fn try_from(code: u32) -> Result<Self, Self::Error> {
		match code {
			0 => Ok(Self::Default),
			1 => Ok(Self::FullRange),
			2 => Ok(Self::LimitedRange),
			_ => Err(()),
		}
	}
}
