//! Screen, window, and application capture via ScreenCaptureKit (macOS), the
//! zero-copy path.
//!
//! `SCStream` delivers IOSurface-backed `CVPixelBuffer`s to an `SCStreamOutput`
//! delegate, the same surface VideoToolbox encodes directly. Content enumeration
//! and capture start are async (completion handlers) and bridged to `await`;
//! per-frame delivery flows through the shared [`FrameChannel`].
//!
//! The three sources differ only in the `SCContentFilter` they build; everything
//! downstream (configuration, delegate, stream, guard) is shared by [`open`].
//!
//! Not covered by tests: capture needs the Screen Recording TCC grant and a real
//! display, so it can't run headless. Exercise it with `moq devices` plus
//! `moq import capture --display/--window/--app`.

use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_core_video::kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
	SCContentFilter, SCDisplay, SCRunningApplication, SCShareableContent, SCStream, SCStreamConfiguration,
	SCStreamDelegate, SCStreamOutput, SCStreamOutputType, SCWindow,
};

use super::surface::surface_frame;
use super::{App, Config, Display, FrameChannel, FrameStream, Window};
use crate::Error;

const DEFAULT_FRAMERATE: i32 = 30;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const ASYNC_TIMEOUT: Duration = Duration::from_secs(5);

/// Only capture normal application windows. Layer 0 is the standard window
/// level; the dock, menu bar, and other chrome live on higher layers and would
/// otherwise flood the list with things nobody means by "a window".
const NORMAL_WINDOW_LAYER: isize = 0;

/// Open a whole-display capture. `device` is a display index (`None` = main).
pub(super) async fn open_display(config: &Config, device: Option<&str>) -> Result<FrameStream, Error> {
	init_core_graphics();
	let content = shareable_content().await?;
	let display = find_display(&content, device)?;

	// An empty exclusion list captures the display as-is, including the desktop
	// background and dock.
	let excluded = NSArray::<SCWindow>::new();
	let filter =
		unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &excluded) };

	let (width, height) = display_size(&display);
	let size = capture_size(width, height);
	open(config, &filter, size).await
}

/// Open a single-window capture. Follows the window as it moves and resizes.
pub(super) async fn open_window(config: &Config, id: &str) -> Result<FrameStream, Error> {
	init_core_graphics();
	let content = shareable_content().await?;
	let window = find_window(&content, id)?;

	let filter = unsafe { SCContentFilter::initWithDesktopIndependentWindow(SCContentFilter::alloc(), &window) };

	let frame = unsafe { window.frame() };
	let size = capture_size(frame.size.width, frame.size.height);
	open(config, &filter, size).await
}

/// Open an application capture: every window owned by `id` (a bundle
/// identifier), including ones opened later.
pub(super) async fn open_app(config: &Config, id: &str) -> Result<FrameStream, Error> {
	init_core_graphics();
	let content = shareable_content().await?;
	let app = find_app(&content, id)?;

	// An application's windows can span displays, but a filter is display-scoped,
	// so the app is captured as it appears on the main display.
	let display = find_display(&content, None)?;
	let apps = NSArray::from_retained_slice(&[app]);
	let excepting = NSArray::<SCWindow>::new();
	let filter = unsafe {
		SCContentFilter::initWithDisplay_includingApplications_exceptingWindows(
			SCContentFilter::alloc(),
			&display,
			&apps,
			&excepting,
		)
	};

	let (width, height) = display_size(&display);
	let size = capture_size(width, height);
	open(config, &filter, size).await
}

/// A display's size in points.
fn display_size(display: &SCDisplay) -> (f64, f64) {
	(unsafe { display.width() } as f64, unsafe { display.height() } as f64)
}

/// Connect the process to the window server.
///
/// `SCContentFilter::initWithDesktopIndependentWindow` asserts on an
/// uninitialized CoreGraphics (`CGS_REQUIRE_INIT`), which aborts the process
/// rather than returning an error. A plain CLI never establishes that
/// connection, and unlike an app there's no AppKit to do it for us; any public
/// CG call does, so make one first. `initWithDisplay:` happens not to need it,
/// which is why display capture survived without this.
fn init_core_graphics() {
	static ONCE: std::sync::Once = std::sync::Once::new();
	ONCE.call_once(|| {
		objc2_core_graphics::CGMainDisplayID();
	});
}

/// Start a stream for `filter`. `size` is the source's native pixel size, used
/// unless the caller overrode width/height.
async fn open(config: &Config, filter: &SCContentFilter, size: (u32, u32)) -> Result<FrameStream, Error> {
	let fps = config.framerate.map(|f| f as i32).unwrap_or(DEFAULT_FRAMERATE).max(1);
	let configuration = unsafe { SCStreamConfiguration::new() };
	// `size` is already even; an override might not be.
	let width = config.width.map(|w| even(w as f64)).unwrap_or(size.0);
	let height = config.height.map(|h| even(h as f64)).unwrap_or(size.1);
	unsafe {
		configuration.setWidth(width as usize);
		configuration.setHeight(height as usize);
		configuration.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
		configuration.setMinimumFrameInterval(CMTime::new(1, fps));
		configuration.setShowsCursor(config.cursor);
	}

	let chan = FrameChannel::new();
	let delegate = Delegate::new(chan.clone());
	let dispatch = DispatchQueue::new("dev.moq.video.screen", None);

	// Register as the stream delegate too, so a stream that dies on its own (the
	// user hit "Stop Sharing", the grant was revoked, the display went away) closes
	// the channel instead of leaving the encode loop parked on a read forever.
	let stream = unsafe {
		let proto = ProtocolObject::from_ref(&*delegate);
		SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), filter, &configuration, Some(proto))
	};
	unsafe {
		let proto = ProtocolObject::from_ref(&*delegate);
		stream
			.addStreamOutput_type_sampleHandlerQueue_error(proto, SCStreamOutputType::Screen, Some(&dispatch))
			.map_err(|e| Error::Codec(anyhow::anyhow!("add screen output: {e:?}")))?;
	}

	start_capture(&stream).await?;

	// The stream keeps capturing until dropped; this guard stops it and closes
	// the channel when the FrameStream goes away.
	let guard = StreamGuard {
		stream,
		chan: chan.clone(),
		_delegate: delegate,
		_dispatch: dispatch,
	};

	let label = config.source.label();
	let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, chan.recv()).await {
		Ok(Some(frame)) => frame,
		Ok(None) | Err(_) => {
			return Err(Error::Codec(anyhow::anyhow!(
				"no frames from {label} within {FIRST_FRAME_TIMEOUT:?} (screen recording permission?)"
			)));
		}
	};
	let (width, height) = (first.width(), first.height());

	tracing::info!(source = %label, width, height, "opened screen capture (ScreenCaptureKit)");

	Ok(FrameStream::new(
		chan,
		width,
		height,
		None,
		label,
		Some(first),
		Box::new(guard),
	))
}

/// List the displays.
pub(super) async fn displays() -> Result<Vec<Display>, Error> {
	let content = shareable_content().await?;
	let displays = unsafe { content.displays() };
	Ok((0..displays.count())
		.map(|index| {
			let display = displays.objectAtIndex(index);
			Display {
				id: index.to_string(),
				name: format!("Display {}", unsafe { display.displayID() }),
				width: unsafe { display.width() } as u32,
				height: unsafe { display.height() } as u32,
			}
		})
		.collect())
}

/// List the on-screen application windows.
pub(super) async fn windows() -> Result<Vec<Window>, Error> {
	let content = shareable_content().await?;
	let windows = unsafe { content.windows() };
	Ok((0..windows.count())
		.map(|index| windows.objectAtIndex(index))
		.filter(|window| unsafe { window.isOnScreen() } && unsafe { window.windowLayer() } == NORMAL_WINDOW_LAYER)
		.map(|window| {
			let frame = unsafe { window.frame() };
			Window {
				id: unsafe { window.windowID() }.to_string(),
				title: unsafe { window.title() }.map(|t| t.to_string()).unwrap_or_default(),
				app: unsafe { window.owningApplication() }
					.map(|app| unsafe { app.applicationName() }.to_string())
					.unwrap_or_default(),
				width: frame.size.width as u32,
				height: frame.size.height as u32,
			}
		})
		.collect())
}

/// List the applications owning at least one on-screen window.
///
/// Derived from the window list rather than `content.applications()`, which also
/// reports background processes that have nothing to capture.
pub(super) async fn apps() -> Result<Vec<App>, Error> {
	let content = shareable_content().await?;
	let windows = unsafe { content.windows() };

	let mut apps = Vec::new();
	let mut seen = std::collections::HashSet::new();
	for index in 0..windows.count() {
		let window = windows.objectAtIndex(index);
		if !unsafe { window.isOnScreen() } || unsafe { window.windowLayer() } != NORMAL_WINDOW_LAYER {
			continue;
		}
		let Some(app) = (unsafe { window.owningApplication() }) else {
			continue;
		};
		let id = unsafe { app.bundleIdentifier() }.to_string();
		if seen.insert(id.clone()) {
			apps.push(App {
				id,
				name: unsafe { app.applicationName() }.to_string(),
			});
		}
	}
	Ok(apps)
}

/// Resolve a display index (`None` = the main display, which ScreenCaptureKit
/// reports first).
fn find_display(content: &SCShareableContent, device: Option<&str>) -> Result<Retained<SCDisplay>, Error> {
	let displays = unsafe { content.displays() };
	// Accept a bare index or the `display:{index}` form a label reports, and
	// reject anything else rather than silently using display 0.
	let index = match device {
		None => 0,
		Some(spec) => spec
			.strip_prefix("display:")
			.unwrap_or(spec)
			.parse::<usize>()
			.map_err(|_| Error::Codec(anyhow::anyhow!("invalid display selector {spec:?}")))?,
	};
	if index >= displays.count() {
		return Err(Error::Codec(anyhow::anyhow!("no display at index {index}")));
	}
	Ok(displays.objectAtIndex(index))
}

/// Resolve a window by the id [`windows`] reports.
fn find_window(content: &SCShareableContent, id: &str) -> Result<Retained<SCWindow>, Error> {
	let wanted: u32 = id
		.parse()
		.map_err(|_| Error::Codec(anyhow::anyhow!("invalid window id {id:?}")))?;

	let windows = unsafe { content.windows() };
	(0..windows.count())
		.map(|index| windows.objectAtIndex(index))
		.find(|window| unsafe { window.windowID() } == wanted)
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("no window with id {id} (did it close?)")))
}

/// Resolve an application by bundle identifier.
fn find_app(content: &SCShareableContent, id: &str) -> Result<Retained<SCRunningApplication>, Error> {
	let apps = unsafe { content.applications() };
	(0..apps.count())
		.map(|index| apps.objectAtIndex(index))
		.find(|app| unsafe { app.bundleIdentifier() }.to_string() == id)
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("no running application with bundle id {id:?}")))
}

/// Capture size for a source measured in points.
///
/// ScreenCaptureKit reports geometry in points, so this captures a retina
/// display at its logical resolution rather than its native pixels: a 2x Mac
/// gives 1710x1106, not 3420x2214. That's what the screen looks like to its
/// owner, and it keeps the derived bitrate sane (the default is a flat
/// bits-per-pixel, so native would quadruple it). Pass `--width`/`--height` for
/// native pixels.
///
/// Rounded down to even: the encoders reject odd dimensions (4:2:0 subsampling
/// halves each axis), and a display can well be an odd number of points tall.
fn capture_size(width: f64, height: f64) -> (u32, u32) {
	(even(width), even(height))
}
/// Round down to a non-zero even number of pixels.
fn even(value: f64) -> u32 {
	((value as u32) & !1).max(2)
}

/// Keeps the capture stream alive; stops it on drop and closes the channel so a
/// parked read returns.
struct StreamGuard {
	stream: Retained<SCStream>,
	chan: Arc<FrameChannel>,
	_delegate: Retained<Delegate>,
	_dispatch: DispatchRetained<DispatchQueue>,
}

impl Drop for StreamGuard {
	fn drop(&mut self) {
		// Fire-and-forget stop; closing the channel unblocks any parked read.
		unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
		self.chan.close();
	}
}

/// Await the async `getShareableContent` to learn the available displays.
async fn shareable_content() -> Result<Retained<SCShareableContent>, Error> {
	let (tx, rx) = tokio::sync::oneshot::channel::<Result<SendObj<SCShareableContent>, String>>();
	let tx = std::sync::Mutex::new(Some(tx));
	let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
		let result = match unsafe { Retained::retain(content) } {
			Some(content) => Ok(SendObj(content)),
			None => Err(error_message(error)),
		};
		if let Some(tx) = tx.lock().unwrap().take() {
			let _ = tx.send(result);
		}
	});
	unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };

	match tokio::time::timeout(ASYNC_TIMEOUT, rx).await {
		Ok(Ok(Ok(content))) => Ok(content.0),
		Ok(Ok(Err(msg))) => Err(Error::Codec(anyhow::anyhow!("shareable content: {msg}"))),
		Ok(Err(_)) => Err(Error::Codec(anyhow::anyhow!("shareable content handler dropped"))),
		Err(_) => Err(Error::Codec(anyhow::anyhow!(
			"timed out listing shareable content (screen recording permission?)"
		))),
	}
}

/// Await the async `startCapture`, surfacing any error.
async fn start_capture(stream: &SCStream) -> Result<(), Error> {
	let (tx, rx) = tokio::sync::oneshot::channel::<Option<String>>();
	let tx = std::sync::Mutex::new(Some(tx));
	let handler = RcBlock::new(move |error: *mut NSError| {
		let result = (!error.is_null()).then(|| error_message(error));
		if let Some(tx) = tx.lock().unwrap().take() {
			let _ = tx.send(result);
		}
	});
	unsafe { stream.startCaptureWithCompletionHandler(Some(&handler)) };

	match tokio::time::timeout(ASYNC_TIMEOUT, rx).await {
		Ok(Ok(None)) => Ok(()),
		Ok(Ok(Some(msg))) => Err(Error::Codec(anyhow::anyhow!("start capture: {msg}"))),
		Ok(Err(_)) => Err(Error::Codec(anyhow::anyhow!("start-capture handler dropped"))),
		Err(_) => Err(Error::Codec(anyhow::anyhow!("timed out starting screen capture"))),
	}
}

fn error_message(error: *mut NSError) -> String {
	match unsafe { error.as_ref() } {
		Some(error) => error.localizedDescription().to_string(),
		None => "unknown error".to_string(),
	}
}

/// Carries a `Retained` from the completion handler's queue to the awaiting
/// task. The objc object is reference-counted and safe to move between threads.
struct SendObj<T>(Retained<T>);
unsafe impl<T> Send for SendObj<T> {}

struct DelegateIvars {
	chan: Arc<FrameChannel>,
}

define_class!(
	#[unsafe(super(NSObject))]
	#[name = "MoqVideoScreenDelegate"]
	#[ivars = DelegateIvars]
	struct Delegate;

	unsafe impl NSObjectProtocol for Delegate {}

	unsafe impl SCStreamDelegate for Delegate {
		#[unsafe(method(stream:didStopWithError:))]
		unsafe fn did_stop(&self, _stream: &SCStream, error: &NSError) {
			tracing::warn!(error = %error.localizedDescription(), "screen capture stopped");
			// Unblocks a parked read; the encode loop then reopens on demand.
			self.ivars().chan.close();
		}
	}

	unsafe impl SCStreamOutput for Delegate {
		#[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
		unsafe fn did_output(&self, _stream: &SCStream, sample_buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
			if kind.0 == SCStreamOutputType::Screen.0 {
				// ScreenCaptureKit calls this at the configured rate even when nothing
				// changed, handing back an image-less buffer; there's no new content to
				// encode, so drop it.
				if let Some(frame) = surface_frame(sample_buffer) {
					self.ivars().chan.push(frame);
				}
			}
		}
	}
);

impl Delegate {
	fn new(chan: Arc<FrameChannel>) -> Retained<Self> {
		let this = Self::alloc().set_ivars(DelegateIvars { chan });
		unsafe { msg_send![super(this), init] }
	}
}
