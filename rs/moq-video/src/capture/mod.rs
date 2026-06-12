//! Frame capture. [`Config`] is shared; the implementation is per-platform and
//! per-source:
//! - macOS camera -> AVFoundation, screen -> ScreenCaptureKit, both yielding
//!   zero-copy `CVPixelBuffer` surfaces straight to VideoToolbox.
//! - other platforms -> [`nokhwa`](https://crates.io/crates/nokhwa) camera
//!   (CPU RGBA -> I420) until a native zero-copy path (V4L2 dmabuf / PipeWire)
//!   lands.
//!
//! [`encode::publish_capture`](crate::encode::publish_capture) consumes [`Config`].

use crate::Error;
use crate::frame::Frame;

#[cfg(target_os = "macos")]
mod avfoundation;
#[cfg(target_os = "macos")]
mod queue;
#[cfg(target_os = "macos")]
mod screencapture;

#[cfg(not(target_os = "macos"))]
mod webcam;

/// What to capture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
	/// A camera / webcam.
	#[default]
	Camera,
	/// A display (whole-screen capture). macOS only for now.
	Display,
}

/// Capture configuration. All fields are hints; the backend picks the closest
/// supported mode.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so
/// new options can be added without breaking callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// What to capture (camera vs display).
	pub source: Source,
	/// Source identifier. `None` opens the default device/display.
	///
	/// For a camera, macOS uses the AVFoundation `uniqueID`; other platforms
	/// take a bare integer (`"0"`) as an index, else a device path/name. For a
	/// display, a bare integer selects by index (default: the main display).
	pub device: Option<String>,
	pub width: Option<u32>,
	pub height: Option<u32>,
	pub framerate: Option<u32>,
}

/// A live frame source, read frame-by-frame. Opened via [`open`].
pub(crate) trait FrameSource {
	/// Block until the next frame, or `None` once the source ends.
	fn read(&mut self) -> Result<Option<Frame>, Error>;
	fn width(&self) -> u32;
	fn height(&self) -> u32;
	/// The negotiated frame rate, or `None` if the source doesn't report one.
	fn framerate(&self) -> Option<u32>;
	fn device(&self) -> &str;
}

/// Open the capture source described by `config`.
pub(crate) fn open(config: &Config) -> Result<Box<dyn FrameSource>, Error> {
	match config.source {
		Source::Camera => {
			#[cfg(target_os = "macos")]
			{
				Ok(Box::new(avfoundation::Camera::open(config)?))
			}
			#[cfg(not(target_os = "macos"))]
			{
				Ok(Box::new(webcam::Camera::open(config)?))
			}
		}
		Source::Display => {
			#[cfg(target_os = "macos")]
			{
				Ok(Box::new(screencapture::Screen::open(config)?))
			}
			#[cfg(not(target_os = "macos"))]
			{
				Err(Error::Codec(anyhow::anyhow!(
					"screen capture is only supported on macOS"
				)))
			}
		}
	}
}
