use std::io;

use crate::video::capture::Parameters as CaptureParameters;
use crate::video::output::Parameters as OutputParameters;
use crate::{
	format::Description as FormatDescription, format::Format, format::FourCC, frameinterval::FrameInterval,
	framesize::FrameSize,
};

/// Capture device protocol
pub trait Capture {
	/// Returns a vector of all frame intervals that the device supports for the given pixel format
	/// and frame size
	fn enum_frameintervals(&self, fourcc: FourCC, width: u32, height: u32) -> io::Result<Vec<FrameInterval>>;

	/// Returns a vector of valid framesizes that the device supports for the given pixel format
	fn enum_framesizes(&self, fourcc: FourCC) -> io::Result<Vec<FrameSize>>;

	/// Returns a vector of valid formats for this device
	///
	/// The "emulated" field describes formats filled in by libv4lconvert.
	/// There may be a conversion related performance penalty when using them.
	fn enum_formats(&self) -> io::Result<Vec<FormatDescription>>;

	/// Returns the format currently in use
	fn format(&self) -> io::Result<Format>;

	/// Modifies the capture format and returns the actual format
	///
	/// The driver tries to match the format parameters on a best effort basis.
	/// Thus, if the combination of format properties cannot be achieved, the closest possible
	/// settings are used and reported back.
	///
	///
	/// # Arguments
	///
	/// * `fmt` - Desired format
	fn set_format(&self, fmt: &Format) -> io::Result<Format>;

	/// Returns the parameters currently in use
	fn params(&self) -> io::Result<CaptureParameters>;

	/// Modifies the capture parameters and returns the actual parameters
	///
	/// # Arguments
	///
	/// * `params` - Desired parameters
	fn set_params(&self, params: &CaptureParameters) -> io::Result<CaptureParameters>;
}

/// Output device protocol
pub trait Output {
	/// Returns a vector of all frame intervals that the device supports for the given pixel format
	/// and frame size
	fn enum_frameintervals(&self, fourcc: FourCC, width: u32, height: u32) -> io::Result<Vec<FrameInterval>>;

	/// Returns a vector of valid framesizes that the device supports for the given pixel format
	fn enum_framesizes(&self, fourcc: FourCC) -> io::Result<Vec<FrameSize>>;

	/// Returns a vector of valid formats for this device
	///
	/// The "emulated" field describes formats filled in by libv4lconvert.
	/// There may be a conversion related performance penalty when using them.
	fn enum_formats(&self) -> io::Result<Vec<FormatDescription>>;

	/// Returns the format currently in use
	fn format(&self) -> io::Result<Format>;

	/// Modifies the capture format and returns the actual format
	///
	/// The driver tries to match the format parameters on a best effort basis.
	/// Thus, if the combination of format properties cannot be achieved, the closest possible
	/// settings are used and reported back.
	///
	/// # Arguments
	///
	/// * `fmt` - Desired format
	fn set_format(&self, fmt: &Format) -> io::Result<Format>;

	/// Returns the parameters currently in use
	fn params(&self) -> io::Result<OutputParameters>;

	/// Modifies the output parameters and returns the actual parameters
	///
	/// # Arguments
	///
	/// * `params` - Desired parameters
	fn set_params(&self, params: &OutputParameters) -> io::Result<OutputParameters>;
}
