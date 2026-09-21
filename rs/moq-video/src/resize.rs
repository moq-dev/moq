//! Frame resizing options.

use crate::Output;

/// Options for [`Frame::resize`](crate::Frame::resize).
///
/// Build with `Config::default()` and set fields, so future options stay
/// additive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// Where the scaled pixels live.
	///
	/// [`Output::Native`] scales a GPU surface on its own device, downloading
	/// and scaling on the CPU only when the device refuses. [`Output::Cpu`]
	/// downloads first and always scales on the CPU.
	pub output: Output,
}
