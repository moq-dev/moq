//! Frame resizing options.

/// Options for [`Frame::resize_with`](crate::Frame::resize_with).
///
/// Build with `Config::default()` and set fields, so future options stay
/// additive.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Config {
	/// Whether to prefer GPU or CPU scaling.
	pub acceleration: Acceleration,
}

/// Which device should scale a frame when both paths are available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Acceleration {
	/// Use the stable platform default.
	///
	/// CUDA and macOS pixel buffers stay on the GPU. Direct3D11 textures use the
	/// CPU because some drivers can block indefinitely when a cold video
	/// processor first runs on a device with a live DXVA decoder.
	#[default]
	Auto,
	/// Always download GPU surfaces and scale on the CPU.
	Cpu,
	/// Scale on the GPU where supported, falling back to the CPU on an error.
	Gpu,
}
