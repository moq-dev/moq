//! System (desktop) audio capture via ScreenCaptureKit (macOS).
//!
//! macOS has no "loopback" input device, so everything the machine is playing is
//! captured through ScreenCaptureKit: the same API as screen capture, configured
//! with `capturesAudio` and an audio-only output. The video side of the stream is
//! still started (SCK has no audio-only mode), so it's pinned to a tiny frame at
//! a low rate and its samples are dropped.
//!
//! Like the mic path, buffers arrive on a dispatch queue and are forwarded to a
//! bounded async channel, so dropping the capture future releases the stream.
//!
//! Not covered by tests: this needs the Screen Recording TCC grant and a real
//! display, so it can't run headless.

use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_core_audio_types::{AudioBuffer, AudioBufferList};
use objc2_core_media::{CMSampleBuffer, CMTime};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
	SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate, SCStreamOutput,
	SCStreamOutputType, SCWindow,
};

use crate::Error;

use super::channel;

/// SCK's audio defaults, used when the caller doesn't pin a format.
const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u32 = 2;

/// The stream still captures video, so ask for the smallest, slowest frame we
/// can and never read it.
const IDLE_VIDEO_SIZE: usize = 2;
const IDLE_VIDEO_FPS: i32 = 1;

const FIRST_BUFFER_TIMEOUT: Duration = Duration::from_secs(5);
const ASYNC_TIMEOUT: Duration = Duration::from_secs(5);

/// One delivered buffer: interleaved `f32` PCM plus the channel count SCK
/// actually used, which `open` checks against the one we asked for.
struct Buffer {
	samples: Vec<f32>,
	channels: u32,
}

/// An open system-audio capture, read buffer-by-buffer via [`read`](Self::read).
pub(crate) struct SystemAudio {
	rx: channel::Receiver<Buffer>,
	/// The first buffer, captured during [`open`](Self::open) so a missing screen
	/// recording grant is an error rather than a silent hang.
	pending: Option<Vec<f32>>,
	_guard: StreamGuard,
}

impl SystemAudio {
	/// The format system audio will be captured at. SCK resamples to whatever we
	/// ask for, so this is exactly what `config` requested (or the defaults), and
	/// needs no open.
	pub(super) fn format(sample_rate: Option<u32>, channels: Option<u32>) -> (u32, u32) {
		(
			sample_rate.unwrap_or(DEFAULT_SAMPLE_RATE),
			channels.unwrap_or(DEFAULT_CHANNELS),
		)
	}

	/// Open (and start) system audio capture.
	pub(super) async fn open(sample_rate: Option<u32>, channels: Option<u32>) -> Result<Self, Error> {
		let (sample_rate, channels) = Self::format(sample_rate, channels);

		// The filter picks which audio is captured; a display filter means
		// "everything playing on that screen", i.e. the whole system.
		let content = shareable_content().await?;
		let displays = unsafe { content.displays() };
		if displays.count() == 0 {
			return Err(Error::Capture("no display to capture system audio from".into()));
		}
		let display = displays.objectAtIndex(0);
		let excluded = NSArray::<SCWindow>::new();
		let filter =
			unsafe { SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &display, &excluded) };

		let configuration = unsafe { SCStreamConfiguration::new() };
		unsafe {
			configuration.setCapturesAudio(true);
			configuration.setSampleRate(sample_rate as isize);
			configuration.setChannelCount(channels as isize);
			// Otherwise anything we play back ourselves would feed straight back in.
			configuration.setExcludesCurrentProcessAudio(true);
			configuration.setWidth(IDLE_VIDEO_SIZE);
			configuration.setHeight(IDLE_VIDEO_SIZE);
			configuration.setMinimumFrameInterval(CMTime::new(1, IDLE_VIDEO_FPS));
		}

		let (tx, mut rx) = channel::bounded::<Buffer>();
		let delegate = Delegate::new(tx);
		let dispatch = DispatchQueue::new("dev.moq.audio.system", None);

		let stream = unsafe {
			SCStream::initWithFilter_configuration_delegate(SCStream::alloc(), &filter, &configuration, None)
		};
		unsafe {
			let proto = ProtocolObject::from_ref(&*delegate);
			stream
				.addStreamOutput_type_sampleHandlerQueue_error(proto, SCStreamOutputType::Audio, Some(&dispatch))
				.map_err(|e| Error::Capture(format!("add audio output: {e:?}")))?;
		}

		start_capture(&stream).await?;

		let guard = StreamGuard {
			stream,
			_delegate: delegate,
			_dispatch: dispatch,
		};

		let pending = match tokio::time::timeout(FIRST_BUFFER_TIMEOUT, rx.recv()).await {
			Ok(Some(buffer)) => buffer,
			Ok(None) => {
				return Err(Error::Capture("system audio stopped before any samples".into()));
			}
			Err(_) => {
				return Err(Error::Capture(format!(
					"no system audio within {FIRST_BUFFER_TIMEOUT:?} (screen recording permission?)"
				)));
			}
		};

		// The catalog is built from the requested format before this opens, so a
		// backend that quietly picked a different layout would hand the encoder
		// wrong-shaped frames. Fail loudly instead.
		if pending.channels != channels {
			return Err(Error::Capture(format!(
				"system audio delivered {} channels, not the {channels} requested",
				pending.channels
			)));
		}

		tracing::info!(sample_rate, channels, "opened system audio (ScreenCaptureKit)");

		Ok(Self {
			rx,
			pending: Some(pending.samples),
			_guard: guard,
		})
	}

	/// Await the next buffer of interleaved `f32` PCM, or `None` once the stream
	/// stops. Cancel-safe: drop the future to stop reading.
	pub(super) async fn read(&mut self) -> Option<Vec<f32>> {
		match self.pending.take() {
			Some(samples) => Some(samples),
			None => Some(self.rx.recv().await?.samples),
		}
	}
}

/// Stops the stream on drop, which also ends the delegate callbacks and so
/// closes the channel.
struct StreamGuard {
	stream: Retained<SCStream>,
	_delegate: Retained<Delegate>,
	_dispatch: DispatchRetained<DispatchQueue>,
}

impl Drop for StreamGuard {
	fn drop(&mut self) {
		unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
	}
}

/// Pull the PCM out of an audio `CMSampleBuffer` as interleaved `f32`, plus the
/// channel count it actually carried.
///
/// SCK hands back non-interleaved (planar) float samples: one buffer per
/// channel. Opus wants them interleaved, so weave them here. Returns `None` if
/// the buffer isn't the float layout we asked for.
fn samples(sample_buffer: &CMSampleBuffer) -> Option<Buffer> {
	// An AudioBufferList is variable-length (a flexible array of AudioBuffer), so
	// ask how big it needs to be, then hand back a buffer of exactly that size.
	let mut needed = 0usize;
	let status = unsafe {
		sample_buffer.audio_buffer_list_with_retained_block_buffer(
			&mut needed,
			std::ptr::null_mut(),
			0,
			None,
			None,
			0,
			std::ptr::null_mut(),
		)
	};
	if status != 0 || needed == 0 {
		return None;
	}

	// Backed by u64, not u8: an AudioBufferList holds a pointer and so needs
	// 8-byte alignment, which a Vec<u8> only guarantees by accident of the
	// allocator.
	let mut storage = vec![0u64; needed.div_ceil(size_of::<u64>())];
	let list = storage.as_mut_ptr().cast::<AudioBufferList>();
	// The block buffer owns the sample data, so it has to outlive the reads below.
	let mut block = std::ptr::null_mut();
	let status = unsafe {
		sample_buffer.audio_buffer_list_with_retained_block_buffer(
			std::ptr::null_mut(),
			list,
			needed,
			None,
			None,
			0,
			&mut block,
		)
	};
	if status != 0 {
		return None;
	}
	// Retained by the call above; take ownership so it's released on return.
	let _block = unsafe { Retained::from_raw(block) };

	let count = unsafe { (*list).mNumberBuffers } as usize;
	if count == 0 {
		return None;
	}
	// `mBuffers` is declared as a 1-element array standing in for a C flexible
	// array member, so walk it as a slice of the real length.
	let buffers =
		unsafe { std::slice::from_raw_parts(std::ptr::addr_of!((*list).mBuffers).cast::<AudioBuffer>(), count) };

	let planes: Vec<&[f32]> = buffers
		.iter()
		.map(|buffer| {
			if buffer.mData.is_null() {
				&[][..]
			} else {
				unsafe {
					std::slice::from_raw_parts(
						buffer.mData.cast::<f32>(),
						buffer.mDataByteSize as usize / size_of::<f32>(),
					)
				}
			}
		})
		.collect();

	// One buffer means it's already interleaved, and that buffer's own
	// mNumberChannels says how many are woven in; otherwise it's one plane per
	// channel and we weave them.
	if let [only] = buffers {
		return Some(Buffer {
			samples: planes[0].to_vec(),
			channels: only.mNumberChannels,
		});
	}

	let frames = planes.iter().map(|plane| plane.len()).min().unwrap_or(0);
	let mut out = Vec::with_capacity(frames * planes.len());
	for frame in 0..frames {
		for plane in &planes {
			out.push(plane[frame]);
		}
	}
	Some(Buffer {
		samples: out,
		channels: planes.len() as u32,
	})
}

struct DelegateIvars {
	/// Closed when the stream stops so a parked `read` returns `None`.
	tx: channel::Sender<Buffer>,
}

define_class!(
	#[unsafe(super(NSObject))]
	#[name = "MoqAudioSystemDelegate"]
	#[ivars = DelegateIvars]
	struct Delegate;

	unsafe impl NSObjectProtocol for Delegate {}

	unsafe impl SCStreamDelegate for Delegate {
		#[unsafe(method(stream:didStopWithError:))]
		unsafe fn did_stop(&self, _stream: &SCStream, error: &NSError) {
			tracing::warn!(error = %error.localizedDescription(), "system audio capture stopped");
			self.ivars().tx.close();
		}
	}

	unsafe impl SCStreamOutput for Delegate {
		#[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
		unsafe fn did_output(&self, _stream: &SCStream, sample_buffer: &CMSampleBuffer, kind: SCStreamOutputType) {
			// The stream also delivers the (unwanted) video frames; ignore them.
			if kind.0 != SCStreamOutputType::Audio.0 {
				return;
			}
			if let Some(buffer) = samples(sample_buffer) {
				self.ivars().tx.push(buffer);
			}
		}
	}
);

impl Delegate {
	fn new(tx: channel::Sender<Buffer>) -> Retained<Self> {
		let this = Self::alloc().set_ivars(DelegateIvars { tx });
		unsafe { msg_send![super(this), init] }
	}
}

/// Await the async `getShareableContent` to find a display to attach to.
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
		Ok(Ok(Err(msg))) => Err(Error::Capture(format!("shareable content: {msg}"))),
		Ok(Err(_)) => Err(Error::Capture("shareable content handler dropped".into())),
		Err(_) => Err(Error::Capture(
			"timed out listing shareable content (screen recording permission?)".into(),
		)),
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
		Ok(Ok(Some(msg))) => Err(Error::Capture(format!("start system audio: {msg}"))),
		Ok(Err(_)) => Err(Error::Capture("start-capture handler dropped".into())),
		Err(_) => Err(Error::Capture("timed out starting system audio".into())),
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
