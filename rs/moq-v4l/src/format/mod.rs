use bitflags::bitflags;
use std::{convert::TryFrom, fmt, mem};

use crate::sys::*;

pub mod colorspace;
pub use colorspace::Colorspace;

pub mod description;
pub use description::Description;

pub mod field;
pub use field::FieldOrder;

pub mod fourcc;
pub use fourcc::FourCC;

pub mod quantization;
pub use quantization::Quantization;

pub mod transfer;
pub use transfer::TransferFunction;

bitflags! {
	#[allow(clippy::unreadable_literal)]
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub struct Flags : u32 {
		const PREMUL_ALPHA  = 0x00000001;
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

#[derive(Debug, Copy, Clone)]
/// Streaming format (single-planar)
pub struct Format {
	/// width in pixels
	pub width: u32,
	/// height in pixels
	pub height: u32,
	/// pixelformat code
	pub fourcc: FourCC,
	/// field order for interlacing
	pub field_order: FieldOrder,

	/// bytes per line
	pub stride: u32,
	/// maximum number of bytes required to store an image
	pub size: u32,

	/// flags set by the application or driver
	pub flags: Flags,

	/// supplements the pixelformat (fourcc) information
	pub colorspace: Colorspace,
	/// the way colors are mapped
	pub quantization: Quantization,
	/// the transfer function for the colorspace
	pub transfer: TransferFunction,
}

impl Format {
	/// Returns a capture format
	///
	/// # Arguments
	///
	/// * `width` - Width in pixels
	/// * `height` - Height in pixels
	/// * `fourcc` - Four character code (pixelformat)
	///
	/// # Example
	///
	/// ```
	/// use moq_v4l::{Format, FourCC};
	/// let fmt = Format::new(640, 480, FourCC::new(b"YUYV"));
	/// ```
	pub const fn new(width: u32, height: u32, fourcc: FourCC) -> Self {
		Format {
			width,
			height,
			fourcc,
			field_order: FieldOrder::Any,
			stride: 0,
			size: 0,
			flags: Flags::empty(),
			colorspace: Colorspace::Default,
			quantization: Quantization::Default,
			transfer: TransferFunction::Default,
		}
	}
}

impl fmt::Display for Format {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		writeln!(f, "width          : {}", self.width)?;
		writeln!(f, "height         : {}", self.height)?;
		writeln!(f, "fourcc         : {}", self.fourcc)?;
		writeln!(f, "field          : {}", self.field_order)?;
		writeln!(f, "stride         : {}", self.stride)?;
		writeln!(f, "size           : {}", self.size)?;
		writeln!(f, "colorspace     : {}", self.colorspace)?;
		writeln!(f, "quantization   : {}", self.quantization)?;
		writeln!(f, "transfer       : {}", self.transfer)?;
		Ok(())
	}
}

impl From<v4l2_pix_format> for Format {
	fn from(fmt: v4l2_pix_format) -> Self {
		Self {
			width: fmt.width,
			height: fmt.height,
			fourcc: FourCC::from(fmt.pixelformat),
			field_order: FieldOrder::try_from(fmt.field).expect("Invalid field order"),
			stride: fmt.bytesperline,
			size: fmt.sizeimage,
			flags: Flags::from(fmt.flags),
			colorspace: Colorspace::try_from(fmt.colorspace).expect("Invalid colorspace"),
			quantization: Quantization::try_from(fmt.quantization).expect("Invalid quantization"),
			transfer: TransferFunction::try_from(fmt.xfer_func).expect("Invalid transfer function"),
		}
	}
}

impl From<Format> for v4l2_pix_format {
	fn from(format: Format) -> Self {
		Self {
			width: format.width,
			height: format.height,
			pixelformat: format.fourcc.into(),
			field: format.field_order as u32,
			bytesperline: format.stride,
			sizeimage: format.size,
			colorspace: format.colorspace as u32,
			flags: format.flags.into(),
			quantization: format.quantization as u32,
			xfer_func: format.transfer as u32,
			..unsafe { mem::zeroed() }
		}
	}
}
