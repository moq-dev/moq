use std::convert::TryFrom;
use std::fmt;

#[derive(Debug, Copy, Clone)]
#[repr(u32)]
/// Transfer function for the colorspace. The driver decides this for capture streams and the user
/// sets it for output streams.
pub enum TransferFunction {
	/// default from the colorspace
	Default = 0,
	/// Rec. 709 transfer function
	Rec709 = 1,
	/// sRGB transfer function
	SRGB = 2,
	/// opRGB transfer function
	OPRGB = 3,
	/// SMPTE 230M transfer function
	SMPTE240M = 4,
	/// No transfer function
	None = 5,
	/// DCI-P3 transfer function
	DCIP3 = 6,
	/// SMPTE 2084 transfer function
	SMPTE2084 = 7,
}

impl fmt::Display for TransferFunction {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Default => write!(f, "default transfer function"),
			Self::Rec709 => write!(f, "Rec. 709 transfer function"),
			Self::SRGB => write!(f, "sRGB transfer function"),
			Self::OPRGB => write!(f, "opRGB transfer function"),
			Self::SMPTE240M => write!(f, "SMPTE 240M transfer function"),
			Self::None => write!(f, "No transfer function"),
			Self::DCIP3 => write!(f, "DCI-P3 transfer function"),
			Self::SMPTE2084 => write!(f, "SMPTE 2084 transfer function"),
		}
	}
}

impl TryFrom<u32> for TransferFunction {
	type Error = ();

	fn try_from(colorspace_code: u32) -> Result<Self, Self::Error> {
		match colorspace_code {
			0 => Ok(Self::Default),
			1 => Ok(Self::Rec709),
			2 => Ok(Self::SRGB),
			3 => Ok(Self::OPRGB),
			4 => Ok(Self::SMPTE240M),
			5 => Ok(Self::None),
			6 => Ok(Self::DCIP3),
			7 => Ok(Self::SMPTE2084),
			_ => Err(()),
		}
	}
}
