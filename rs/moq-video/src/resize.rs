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
	/// Use the platform default.
	///
	/// GPU-backed surfaces stay on the GPU on every supported platform.
	#[default]
	Auto,
	/// Always download GPU surfaces and scale on the CPU.
	Cpu,
	/// Scale on the GPU where supported, falling back to the CPU on an error.
	Gpu,
}
