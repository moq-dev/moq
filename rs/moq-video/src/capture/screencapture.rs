//! Screen capture via ScreenCaptureKit (macOS), the zero-copy path.
//!
//! `SCStream` delivers IOSurface-backed `CVPixelBuffer`s to an `SCStreamOutput`
//! delegate, the same surface VideoToolbox encodes directly. Content enumeration
//! and capture start are async (completion handlers) and bridged to `await`;
//! per-frame delivery flows through the shared [`FrameChannel`].
//!
//! Compile-verified only: screen capture needs the Screen Recording TCC grant
//! and a display, so it can't run in a headless test. ScreenCaptureKit may also
//! expect a run loop for its completion handlers; validate in a real app.

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
	SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamOutput, SCStreamOutputType, SCWindow,
};

use super::surface::surface_frame;
use super::{Config, FrameChannel, FrameStream};
use crate::Error;

const DEFAULT_FRAMERATE: i32 = 30;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const ASYNC_TIMEOUT: Duration = Duration::from_secs(5);

/// Open a display capture and stream its frames.
pub(super) async fn open(config: &Config) -> Result<FrameStream, Error> {
	let content = shareable_content().await?;
	let displays = unsafe { content.displays() };
	// Accept a bare index or the `display:{index}` form that `device()` reports,
	// and reject anything else rather than silently using display 0.
	let index = match config.device.as_deref() {
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
	let display = displays.objectAtIndex(index);
	let display_id = unsafe { display.displayID() };

	let windows = NSArray::<SCWindow>::new();
	let filter =
		unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &windows) };

	let fps = config.framerate.map(|f| f as i32).unwrap_or(DEFAULT_FRAMERATE).max(1);
	let configuration = unsafe { SCStreamConfiguration::new() };
	unsafe {
		configuration.setWidth(config.width.map(|w| w as usize).unwrap_or(display.width() as usize));
		configuration.setHeight(config.height.map(|h| h as usize).unwrap_or(display.height() as usize));
		configuration.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
		configuration.setMinimumFrameInterval(CMTime::new(1, fps));
		configuration.setShowsCursor(true);
	}

	let chan = FrameChannel::new();
	let delegate = Delegate::new(chan.clone());
	let dispatch = DispatchQueue::new("dev.moq.video.screen", None);

	let stream =
		unsafe { SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &configuration, None) };
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

	let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, chan.recv()).await {
		Ok(Some(frame)) => frame,
		Ok(None) | Err(_) => {
			return Err(Error::Codec(anyhow::anyhow!(
				"no frames from display within {FIRST_FRAME_TIMEOUT:?} (screen recording permission?)"
			)));
		}
	};
	let (width, height) = (first.width(), first.height());

	tracing::info!(
		display = display_id,
		width,
		height,
		"opened screen capture (ScreenCaptureKit)"
	);

	Ok(FrameStream::new(
		chan,
		width,
		height,
		None,
		format!("display:{index}"),
		Some(first),
		Box::new(guard),
	))
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
			"timed out listing displays (screen recording permission?)"
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

	unsafe impl SCStreamOutput for Delegate {
		#[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
		unsafe fn did_output(&self, _stream: &SCStream, sample_buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
			if kind.0 == SCStreamOutputType::Screen.0 {
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
