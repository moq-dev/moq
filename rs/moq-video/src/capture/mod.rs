//! Frame capture. [`Config`] is shared; the implementation is per-platform and
//! per-source:
//! - macOS camera -> AVFoundation, screen -> ScreenCaptureKit, both yielding
//!   zero-copy `CVPixelBuffer` surfaces straight to VideoToolbox.
//! - Linux camera -> native V4L2 (YUYV / MJPEG -> CPU I420).
//! - Windows camera -> native Media Foundation (`IMFSourceReader` -> CPU I420).
//!
//! [`encode::publish_capture`](crate::encode::publish_capture) consumes [`Config`].

use std::sync::Arc;

use crate::Error;
use crate::frame::Frame;

mod channel;
use channel::FrameChannel;

#[cfg(target_os = "macos")]
mod avfoundation;
#[cfg(target_os = "macos")]
mod screencapture;
#[cfg(target_os = "macos")]
mod surface;

// Native V4L2 camera capture on Linux.
#[cfg(target_os = "linux")]
mod v4l2;

// Native Media Foundation camera capture on Windows.
#[cfg(target_os = "windows")]
mod mediafoundation;

// Blocking-device -> async-channel bridge used by V4L2 / Media Foundation.
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod pump;

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

/// A live, async frame source opened via [`open`].
///
/// Every backend delivers frames through a shared [`FrameChannel`], so the
/// encode loop just `read().await`s regardless of platform. Dropping the stream
/// releases the device (stops the macOS `AVCaptureSession`, joins the V4L2 /
/// Media Foundation pump thread). That is the whole point: because `read` is a
/// real await, cancelling the capture future drops this and the camera turns off
/// promptly, with no blocking task left pinned to the runtime.
pub(crate) struct FrameStream {
	chan: Arc<FrameChannel>,
	width: u32,
	height: u32,
	framerate: Option<u32>,
	device: String,
	/// First frame captured during [`open`] (some backends learn their geometry
	/// only from a frame); returned by the first [`read`](Self::read).
	pending: Option<Frame>,
	/// Keeps the backend alive and releases it on drop. Type-erased because it
	/// differs per platform (objc session + delegate, or pump-thread guard).
	_backend: Box<dyn std::any::Any>,
}

impl FrameStream {
	/// Build a stream from a backend's channel, geometry, and keep-alive guard.
	fn new(
		chan: Arc<FrameChannel>,
		width: u32,
		height: u32,
		framerate: Option<u32>,
		device: String,
		pending: Option<Frame>,
		backend: Box<dyn std::any::Any>,
	) -> Self {
		Self {
			chan,
			width,
			height,
			framerate,
			device,
			pending,
			_backend: backend,
		}
	}

	/// Await the next frame, or `None` once the source ends. Cancel-safe: drop
	/// the future to stop reading and release the device.
	pub(crate) async fn read(&mut self) -> Option<Frame> {
		if let Some(frame) = self.pending.take() {
			return Some(frame);
		}
		self.chan.recv().await
	}

	pub(crate) fn width(&self) -> u32 {
		self.width
	}

	pub(crate) fn height(&self) -> u32 {
		self.height
	}

	/// The negotiated frame rate, or `None` if the source doesn't report one.
	pub(crate) fn framerate(&self) -> Option<u32> {
		self.framerate
	}

	pub(crate) fn device(&self) -> &str {
		&self.device
	}
}

/// Open the capture source described by `config`.
pub(crate) async fn open(config: &Config) -> Result<FrameStream, Error> {
	match config.source {
		Source::Camera => {
			#[cfg(target_os = "macos")]
			{
				avfoundation::open(config).await
			}
			#[cfg(target_os = "linux")]
			{
				v4l2::open(config).await
			}
			#[cfg(target_os = "windows")]
			{
				mediafoundation::open(config).await
			}
			#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
			{
				Err(Error::Codec(anyhow::anyhow!(
					"camera capture is not supported on this platform"
				)))
			}
		}
		Source::Display => {
			#[cfg(target_os = "macos")]
			{
				screencapture::open(config).await
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
