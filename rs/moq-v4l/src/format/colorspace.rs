use std::convert::TryFrom;
use std::fmt;

#[derive(Debug, Copy, Clone)]
#[repr(u32)]
/// Colorspace for pixels.
///
/// The driver decides this for capture streams and the user sets it for
/// output streams.
pub enum Colorspace {
	/// driver will pick default
	Default = 0,
	/// SMPTE 170M
	SMPTE170M = 1,
	/// SMPTE 240M
	SMPTE240M = 2,
	/// Rec. 709, aka BT.709
	Rec709 = 3,
	// BT878=4: deprecated, no driver returns this
	/// NTSC
	NTSC = 5,
	/// EBU Tech 3213
	EBUTech3212 = 6,
	/// use for JPEGs: sRGB colorspace, YCbCr encoding, and full range quantization
	JPEG = 7,
	/// sRGB
	SRGB = 8,
	/// opRGB
	OPRGB = 9,
	/// Rec. 2020, aka BT.2020
	Rec2020 = 10,
	/// for RAW images
	RAW = 11,
	/// DCI-P3
	DCIP3 = 12,
}

impl fmt::Display for Colorspace {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Default => write!(f, "default"),
			Self::SMPTE170M => write!(f, "SMPTE 170M"),
			Self::SMPTE240M => write!(f, "SMPTE 240M"),
			Self::Rec709 => write!(f, "Rec. 709"),
			Self::NTSC => write!(f, "NTSC"),
			Self::EBUTech3212 => write!(f, "EBU Tech 3213"),
			Self::JPEG => write!(f, "JPEG"),
			Self::SRGB => write!(f, "sRGB"),
			Self::OPRGB => write!(f, "opRGB"),
			Self::Rec2020 => write!(f, "Rec. 2020"),
			Self::RAW => write!(f, "RAW"),
			Self::DCIP3 => write!(f, "DCI-P3"),
		}
	}
}

impl TryFrom<u32> for Colorspace {
	type Error = ();

	fn try_from(code: u32) -> Result<Self, Self::Error> {
		match code {
			0 => Ok(Self::Default),
			1 => Ok(Self::SMPTE170M),
			2 => Ok(Self::SMPTE240M),
			3 => Ok(Self::Rec709),
			5 => Ok(Self::NTSC),
			6 => Ok(Self::EBUTech3212),
			7 => Ok(Self::JPEG),
			8 => Ok(Self::SRGB),
			9 => Ok(Self::OPRGB),
			10 => Ok(Self::Rec2020),
			11 => Ok(Self::RAW),
			12 => Ok(Self::DCIP3),
			_ => Err(()),
		}
	}
}
