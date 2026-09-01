//! Surface capture. [`Config`] is shared; the implementation is per-platform and
//! per-source:
//! - macOS camera -> AVFoundation, screen -> ScreenCaptureKit, both yielding
//!   zero-copy `CVPixelBuffer` surfaces straight to VideoToolbox.
//! - Linux camera -> native V4L2 (YUYV / MJPEG -> CPU I420), X11 display and
//!   window -> X11, Wayland display -> xdg-desktop-portal + PipeWire
//!   (`pipewire` feature).
//! - Windows camera -> native Media Foundation (`IMFSourceReader`), screen ->
//!   DXGI Desktop Duplication, window -> GDI (BGRA -> CPU I420).
//!
//! [`encode::publish_capture`](crate::encode::publish_capture) consumes [`Config`].

use std::sync::Arc;

use crate::Error;
use crate::frame::Surface;

const MAX_FRAMERATE: u32 = 1_000_000;

mod channel;
use channel::FrameChannel;

/// Type-erased keep-alive for a capture backend, dropped to release the device.
///
/// `Send` off macOS (the backend is a pump-thread guard: an `Arc` stop flag plus
/// a `JoinHandle`), which keeps [`publish_capture`](crate::encode::publish_capture)
/// `Send` so a server can `tokio::spawn` it. On macOS the backend is the objc
/// `AVCaptureSession` (plus its delegate), which is `!Send`, so that platform's
/// capture future is `!Send` too.
#[cfg(not(target_os = "macos"))]
type Keepalive = Box<dyn std::any::Any + Send>;
#[cfg(target_os = "macos")]
type Keepalive = Box<dyn std::any::Any>;

#[cfg(target_os = "macos")]
mod avfoundation;
#[cfg(target_os = "macos")]
mod screencapture;
#[cfg(target_os = "macos")]
mod surface;

// Native V4L2 camera capture on Linux.
#[cfg(target_os = "linux")]
mod v4l2;
// Native X11 display and window capture, including the portal fallback.
#[cfg(target_os = "linux")]
mod x11;

// Portal + PipeWire screen capture on Linux.
#[cfg(all(target_os = "linux", feature = "pipewire"))]
mod pipewire;

// Native Media Foundation camera capture on Windows.
#[cfg(target_os = "windows")]
mod mediafoundation;

// DXGI Desktop Duplication screen capture on Windows.
#[cfg(target_os = "windows")]
mod desktopduplication;
// Native GDI window enumeration and capture on Windows.
#[cfg(target_os = "windows")]
mod window;

// Blocking-device -> async-channel bridge used by V4L2 / Media Foundation.
#[cfg(any(target_os = "linux", target_os = "windows"))]
mod pump;

/// What to capture. Each variant carries the identifier that selects it, so a
/// window can't be captured without saying which one, and a camera id can't
/// reach the display backend.
///
/// The identifiers come from [`cameras`], [`displays`], [`windows`], and
/// [`apps`]; each listed item's `source()` builds the matching variant.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Source {
	/// A camera / webcam. `None` opens the default camera.
	///
	/// The identifiers from [`cameras`] are an AVFoundation `uniqueID` on macOS,
	/// a `/dev/videoN` path on Linux, and a Media Foundation symbolic link on
	/// Windows. Bare numeric indices remain accepted on Linux and Windows.
	Camera(Option<String>),

	/// A whole display. `None` opens the main display.
	///
	/// The id is the opaque value [`displays`] reports. On Wayland the
	/// xdg-desktop-portal picker owns selection and no stable id is available.
	Display(Option<String>),

	/// A single window, by the id [`windows`] reports. Supported on macOS,
	/// Windows, and X11.
	Window(String),

	/// Every window belonging to one application, by the id [`apps`] reports
	/// (a bundle identifier). Windows that open later are included. macOS only.
	App(String),
}

/// The default camera, matching the historical `Config::default()`.
impl Default for Source {
	fn default() -> Self {
		Self::Camera(None)
	}
}

impl Source {
	/// A short human-readable name for the source, used in logs and as the
	/// captured device label.
	///
	/// macOS-only: one ScreenCaptureKit backend serves display, window, and app,
	/// so it names the source from the config. The other backends label a stream
	/// with the device they resolved (`/dev/video0`, a Media Foundation friendly
	/// name), which the config doesn't know.
	#[cfg(target_os = "macos")]
	pub(crate) fn label(&self) -> String {
		match self {
			Self::Camera(None) => "camera".to_string(),
			Self::Camera(Some(id)) => format!("camera:{id}"),
			Self::Display(None) => "display".to_string(),
			Self::Display(Some(id)) => format!("display:{id}"),
			Self::Window(id) => format!("window:{id}"),
			Self::App(id) => format!("app:{id}"),
		}
	}
}

/// A camera reported by [`cameras`].
#[derive(Clone, Debug)]
pub struct Camera {
	/// Opaque identifier: pass to [`Source::Camera`].
	pub id: String,
	/// Human-readable name, e.g. "FaceTime HD Camera".
	pub name: String,
}

impl Camera {
	/// The [`Source`] that captures this camera.
	pub fn source(&self) -> Source {
		Source::Camera(Some(self.id.clone()))
	}
}

/// A display reported by [`displays`].
#[derive(Clone, Debug)]
pub struct Display {
	/// Opaque identifier: pass to [`Source::Display`].
	pub id: String,
	/// Human-readable name, e.g. "Display 1".
	pub name: String,
	/// Width in the platform's desktop coordinate space: points on macOS and
	/// desktop pixels on Windows and X11.
	pub width: u32,
	/// Height in the platform's desktop coordinate space: points on macOS and
	/// desktop pixels on Windows and X11.
	pub height: u32,
}

impl Display {
	/// The [`Source`] that captures this display.
	pub fn source(&self) -> Source {
		Source::Display(Some(self.id.clone()))
	}
}

/// A window reported by [`windows`].
#[derive(Clone, Debug)]
pub struct Window {
	/// Opaque identifier: pass to [`Source::Window`].
	pub id: String,
	/// The window title, empty if it has none.
	pub title: String,
	/// The name of the application owning the window.
	pub app: String,
	/// Width in platform window coordinates: points on macOS and pixels on
	/// Windows and X11.
	pub width: u32,
	/// Height in platform window coordinates: points on macOS and pixels on
	/// Windows and X11.
	pub height: u32,
}

impl Window {
	/// The [`Source`] that captures this window.
	pub fn source(&self) -> Source {
		Source::Window(self.id.clone())
	}
}

/// An application reported by [`apps`].
#[derive(Clone, Debug)]
pub struct App {
	/// Bundle identifier: pass to [`Source::App`].
	pub id: String,
	/// Human-readable name, e.g. "Safari".
	pub name: String,
}

impl App {
	/// The [`Source`] that captures every window of this application.
	pub fn source(&self) -> Source {
		Source::App(self.id.clone())
	}
}

/// Capture configuration. All fields are hints; the backend picks the closest
/// supported mode.
///
/// `#[non_exhaustive]`: construct via [`Config::default`] and set fields, so
/// new options can be added without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// What to capture.
	pub source: Source,
	/// Preferred output width in pixels.
	pub width: Option<u32>,
	/// Preferred output height in pixels.
	pub height: Option<u32>,
	/// Preferred frame rate in frames per second, from 1 through 1,000,000.
	pub framerate: Option<u32>,
	/// Draw the mouse cursor into captured frames. Screen/window/app sources
	/// only; ignored by cameras. Defaults to `true`.
	pub cursor: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			source: Source::default(),
			width: None,
			height: None,
			framerate: None,
			cursor: true,
		}
	}
}

/// A live, async frame source opened via [`open`].
///
/// Every backend delivers frames through the same latest-frame channel, so the
/// consumer just `read().await`s regardless of platform. Dropping the stream
/// releases the device (stops the macOS `AVCaptureSession`, joins the V4L2 /
/// Media Foundation pump thread). That is the whole point: because `read` is a
/// real await, cancelling the capture future drops this and the camera turns off
/// promptly, with no blocking task left pinned to the runtime.
pub struct Stream {
	chan: Arc<FrameChannel>,
	width: u32,
	height: u32,
	framerate: Option<u32>,
	color: Option<crate::Color>,
	label: String,
	/// First frame captured during [`open`] (some backends learn their geometry
	/// only from a frame); returned by the first [`read`](Self::read).
	pending: Option<Surface>,
	/// Keeps the backend alive and releases it on drop. Type-erased because it
	/// differs per platform (objc session + delegate, or pump-thread guard).
	_backend: Keepalive,
}

impl Stream {
	/// Build a stream from a backend's channel, geometry, and keep-alive guard.
	fn new(
		chan: Arc<FrameChannel>,
		width: u32,
		height: u32,
		framerate: Option<u32>,
		label: String,
		pending: Option<Surface>,
		backend: Keepalive,
	) -> Self {
		let color = pending.as_ref().and_then(Surface::color);
		Self {
			chan,
			width,
			height,
			framerate,
			color,
			label,
			pending,
			_backend: backend,
		}
	}

	/// Await the next frame, or `None` once the source ends.
	///
	/// `None` is recoverable: the source stopped for a benign reason (it
	/// resized, the compositor renegotiated the format, the device went idle)
	/// and reopening is expected to work. An `Err` is terminal for this
	/// selection: the source is gone or was refused.
	///
	/// Dropping this future cancels only the pending read. Dropping the stream
	/// releases the capture source.
	pub async fn read(&mut self) -> Result<Option<Surface>, Error> {
		if let Some(frame) = self.pending.take() {
			return Ok(Some(frame));
		}
		self.chan.recv().await
	}

	/// Width of the captured frames in pixels.
	pub fn width(&self) -> u32 {
		self.width
	}

	/// Height of the captured frames in pixels.
	pub fn height(&self) -> u32 {
		self.height
	}

	/// The negotiated frame rate, or `None` if the source doesn't report one.
	pub fn framerate(&self) -> Option<u32> {
		self.framerate
	}

	/// The first frame's declared color space, when its capture backend knows it.
	pub fn color(&self) -> Option<crate::Color> {
		self.color
	}

	/// Human-readable label for the selected capture source.
	pub fn label(&self) -> &str {
		&self.label
	}
}

/// Open the capture source described by `config`.
pub async fn open(config: &Config) -> Result<Stream, Error> {
	validate(config)?;
	match &config.source {
		Source::Camera(device) => {
			let _ = device;
			#[cfg(target_os = "macos")]
			{
				avfoundation::open(config, device.as_deref()).await
			}
			#[cfg(target_os = "linux")]
			{
				v4l2::open(config, device.as_deref()).await
			}
			#[cfg(target_os = "windows")]
			{
				mediafoundation::open(config, device.as_deref()).await
			}
			#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
			{
				Err(Error::Unsupported("camera capture".to_string()))
			}
		}
		Source::Display(device) => {
			let _ = device;
			#[cfg(target_os = "macos")]
			{
				screencapture::open_display(config, device.as_deref()).await
			}
			#[cfg(target_os = "windows")]
			{
				desktopduplication::open(config, device.as_deref()).await
			}
			#[cfg(all(target_os = "linux", feature = "pipewire"))]
			{
				if x11::selected(device.as_deref()) {
					x11::open_display(config, device.as_deref()).await
				} else {
					pipewire::open(config, device.as_deref()).await
				}
			}
			#[cfg(all(target_os = "linux", not(feature = "pipewire")))]
			{
				x11::open_display(config, device.as_deref()).await
			}
			#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
			{
				Err(Error::Unsupported("screen capture".to_string()))
			}
		}
		Source::Window(id) => {
			let _ = id;
			#[cfg(target_os = "macos")]
			{
				screencapture::open_window(config, id).await
			}
			#[cfg(target_os = "linux")]
			{
				x11::open_window(config, id).await
			}
			#[cfg(target_os = "windows")]
			{
				window::open(config, id).await
			}
			#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
			{
				Err(Error::Unsupported("window capture".to_string()))
			}
		}
		Source::App(id) => {
			let _ = id;
			#[cfg(target_os = "macos")]
			{
				screencapture::open_app(config, id).await
			}
			#[cfg(not(target_os = "macos"))]
			{
				Err(Error::Unsupported("application capture".to_string()))
			}
		}
	}
}

fn validate(config: &Config) -> Result<(), Error> {
	match config.framerate {
		Some(value) if value == 0 || value > MAX_FRAMERATE => Err(Error::InvalidFramerate(value)),
		_ => Ok(()),
	}
}

/// List the available cameras and the identifiers [`Source::Camera`] accepts.
pub async fn cameras() -> Result<Vec<Camera>, Error> {
	#[cfg(target_os = "macos")]
	{
		avfoundation::cameras()
	}
	#[cfg(target_os = "linux")]
	{
		blocking(v4l2::cameras).await
	}
	#[cfg(target_os = "windows")]
	{
		blocking(mediafoundation::cameras).await
	}
	#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
	{
		Err(Error::Unsupported("listing cameras".to_string()))
	}
}

/// List the available displays and the identifiers [`Source::Display`] accepts.
///
/// On Wayland the xdg-desktop-portal picker owns display selection, so there is
/// no portal list or stable identifier to expose. When XWayland is available,
/// its native displays are listed with stable X11 ids.
pub async fn displays() -> Result<Vec<Display>, Error> {
	#[cfg(target_os = "macos")]
	{
		screencapture::displays().await
	}
	#[cfg(target_os = "windows")]
	{
		blocking(desktopduplication::displays).await
	}
	#[cfg(target_os = "linux")]
	{
		blocking(x11::displays).await
	}
	#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
	{
		Err(Error::Unsupported("listing displays".to_string()))
	}
}

/// List the on-screen windows.
pub async fn windows() -> Result<Vec<Window>, Error> {
	#[cfg(target_os = "macos")]
	{
		screencapture::windows().await
	}
	#[cfg(target_os = "linux")]
	{
		blocking(x11::windows).await
	}
	#[cfg(target_os = "windows")]
	{
		blocking(window::windows).await
	}
	#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
	{
		Err(Error::Unsupported("listing windows".to_string()))
	}
}

/// List the applications with at least one on-screen window. macOS only.
pub async fn apps() -> Result<Vec<App>, Error> {
	#[cfg(target_os = "macos")]
	{
		screencapture::apps().await
	}
	#[cfg(not(target_os = "macos"))]
	{
		Err(Error::Unsupported("listing applications".to_string()))
	}
}

/// Run synchronous platform enumeration off the async runtime's worker threads.
#[cfg(any(target_os = "linux", target_os = "windows"))]
async fn blocking<T, F>(f: F) -> Result<T, Error>
where
	F: FnOnce() -> Result<T, Error> + Send + 'static,
	T: Send + 'static,
{
	tokio::task::spawn_blocking(f)
		.await
		.map_err(|err| Error::Codec(anyhow::anyhow!("capture enumeration thread failed: {err}")))?
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn validates_framerate_range() {
		let mut config = Config::default();
		assert!(validate(&config).is_ok());

		config.framerate = Some(1);
		assert!(validate(&config).is_ok());
		config.framerate = Some(MAX_FRAMERATE);
		assert!(validate(&config).is_ok());

		config.framerate = Some(0);
		assert!(matches!(validate(&config), Err(Error::InvalidFramerate(0))));
		config.framerate = Some(MAX_FRAMERATE + 1);
		assert!(matches!(validate(&config), Err(Error::InvalidFramerate(value)) if value == MAX_FRAMERATE + 1));
	}
}
