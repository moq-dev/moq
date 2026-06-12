//! Screen capture via ScreenCaptureKit (macOS), the zero-copy path.
//!
//! `SCStream` delivers IOSurface-backed `CVPixelBuffer`s to an `SCStreamOutput`
//! delegate, the same surface VideoToolbox encodes directly. Content enumeration
//! and capture start are async (completion handlers), bridged to a blocking
//! `open` with a channel; per-frame delivery uses the shared [`super::queue`].
//!
//! Compile-verified only: screen capture needs the Screen Recording TCC grant
//! and a display, so it can't run in a headless test. ScreenCaptureKit may also
//! expect a run loop for its completion handlers; validate in a real app.

use std::sync::Arc;
use std::sync::mpsc;
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

use super::queue::{FrameQueue, surface_frame};
use super::{Config, FrameSource};
use crate::Error;
use crate::frame::Frame;

const DEFAULT_FRAMERATE: i32 = 30;
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);
const ASYNC_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) struct Screen {
	stream: Retained<SCStream>,
	queue: Arc<FrameQueue>,
	_delegate: Retained<Delegate>,
	_dispatch: DispatchRetained<DispatchQueue>,
	width: u32,
	height: u32,
	device: String,
	pending: Option<Frame>,
}

impl Screen {
	pub(super) fn open(config: &Config) -> Result<Self, Error> {
		let content = shareable_content()?;
		let displays = unsafe { content.displays() };
		let index = config
			.device
			.as_deref()
			.and_then(|d| d.parse::<usize>().ok())
			.unwrap_or(0);
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

		let queue = FrameQueue::new();
		let delegate = Delegate::new(queue.clone());
		let dispatch = DispatchQueue::new("dev.moq.video.screen", None);

		let stream = unsafe {
			SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &configuration, None)
		};
		unsafe {
			let proto = ProtocolObject::from_ref(&*delegate);
			stream
				.addStreamOutput_type_sampleHandlerQueue_error(proto, SCStreamOutputType::Screen, Some(&dispatch))
				.map_err(|e| Error::Codec(anyhow::anyhow!("add screen output: {e:?}")))?;
		}

		start_capture(&stream)?;

		let first = queue.pop_timeout(FIRST_FRAME_TIMEOUT).ok_or_else(|| {
			Error::Codec(anyhow::anyhow!(
				"no frames from display within {FIRST_FRAME_TIMEOUT:?} (screen recording permission?)"
			))
		})?;
		let (width, height) = (first.width(), first.height());

		tracing::info!(
			display = display_id,
			width,
			height,
			"opened screen capture (ScreenCaptureKit)"
		);

		Ok(Self {
			stream,
			queue,
			_delegate: delegate,
			_dispatch: dispatch,
			width,
			height,
			device: format!("display:{display_id}"),
			pending: Some(first),
		})
	}
}

impl FrameSource for Screen {
	fn read(&mut self) -> Result<Option<Frame>, Error> {
		if let Some(frame) = self.pending.take() {
			return Ok(Some(frame));
		}
		Ok(self.queue.pop())
	}

	fn width(&self) -> u32 {
		self.width
	}

	fn height(&self) -> u32 {
		self.height
	}

	fn framerate(&self) -> Option<u32> {
		None
	}

	fn device(&self) -> &str {
		&self.device
	}
}

impl Drop for Screen {
	fn drop(&mut self) {
		// Fire-and-forget stop; the queue closes so `read` unblocks immediately.
		unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
		self.queue.close();
	}
}

/// Block on the async `getShareableContent` to learn the available displays.
fn shareable_content() -> Result<Retained<SCShareableContent>, Error> {
	let (tx, rx) = mpsc::channel::<Result<SendObj<SCShareableContent>, String>>();
	let handler = RcBlock::new(move |content: *mut SCShareableContent, error: *mut NSError| {
		let result = match unsafe { Retained::retain(content) } {
			Some(content) => Ok(SendObj(content)),
			None => Err(error_message(error)),
		};
		let _ = tx.send(result);
	});
	unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&handler) };

	match rx.recv_timeout(ASYNC_TIMEOUT) {
		Ok(Ok(content)) => Ok(content.0),
		Ok(Err(msg)) => Err(Error::Codec(anyhow::anyhow!("shareable content: {msg}"))),
		Err(_) => Err(Error::Codec(anyhow::anyhow!(
			"timed out listing displays (screen recording permission?)"
		))),
	}
}

/// Block on the async `startCapture`, surfacing any error.
fn start_capture(stream: &SCStream) -> Result<(), Error> {
	let (tx, rx) = mpsc::channel::<Option<String>>();
	let handler = RcBlock::new(move |error: *mut NSError| {
		let _ = tx.send((!error.is_null()).then(|| error_message(error)));
	});
	unsafe { stream.startCaptureWithCompletionHandler(Some(&handler)) };

	match rx.recv_timeout(ASYNC_TIMEOUT) {
		Ok(None) => Ok(()),
		Ok(Some(msg)) => Err(Error::Codec(anyhow::anyhow!("start capture: {msg}"))),
		Err(_) => Err(Error::Codec(anyhow::anyhow!("timed out starting screen capture"))),
	}
}

fn error_message(error: *mut NSError) -> String {
	match unsafe { error.as_ref() } {
		Some(error) => error.localizedDescription().to_string(),
		None => "unknown error".to_string(),
	}
}

/// Carries a `Retained` from the completion handler's queue to `open`'s thread.
/// The objc object is reference-counted and safe to move between threads.
struct SendObj<T>(Retained<T>);
unsafe impl<T> Send for SendObj<T> {}

struct DelegateIvars {
	queue: Arc<FrameQueue>,
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
					self.ivars().queue.push(frame);
				}
			}
		}
	}
);

impl Delegate {
	fn new(queue: Arc<FrameQueue>) -> Retained<Self> {
		let this = Self::alloc().set_ivars(DelegateIvars { queue });
		unsafe { msg_send![super(this), init] }
	}
}
