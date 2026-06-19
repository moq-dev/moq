//! Camera capture via AVFoundation (macOS), the zero-copy path.
//!
//! `AVCaptureVideoDataOutput` delivers IOSurface-backed `CVPixelBuffer`s on a
//! dispatch queue; we wrap each as a [`Frame::Surface`] and hand it to
//! VideoToolbox with no copy and no color conversion. The push-style delegate is
//! bridged to the pull-style [`FrameSource::read`] through [`super::queue`].

use std::sync::Arc;
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
	AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput,
	AVCaptureSession, AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaType,
	AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSObject, NSObjectProtocol, NSString};

use super::queue::{FrameQueue, surface_frame};
use super::{Config, FrameSource};
use crate::Error;
use crate::frame::Frame;

/// How long `open` waits for the first frame before assuming the camera never
/// started (e.g. permission denied).
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the user to answer the camera-permission prompt the
/// first time capture runs.
const ACCESS_TIMEOUT: Duration = Duration::from_secs(60);

/// Ensure the process is authorized to use the camera, prompting once if the
/// decision hasn't been made yet.
///
/// macOS otherwise vends black/no frames for an unauthorized client, which
/// surfaces as the confusing [`FIRST_FRAME_TIMEOUT`] hang. Requesting up front
/// turns "denied" into an immediate, actionable error and blocks on the system
/// prompt while the user decides. Note the prompt is attributed to the
/// responsible app (the one that launched the process), so a bare CLI inherits
/// its host app's grant.
fn ensure_camera_access(media: &AVMediaType) -> Result<(), Error> {
	let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };

	if status == AVAuthorizationStatus::Authorized {
		return Ok(());
	}
	if status == AVAuthorizationStatus::NotDetermined {
		// requestAccess returns asynchronously on an arbitrary queue; block the
		// (already off-runtime) capture-open thread on its result so we don't fall
		// through to a black-frame first-frame timeout while the prompt is up.
		let (tx, rx) = std::sync::mpsc::channel();
		let handler = RcBlock::new(move |granted: Bool| {
			let _ = tx.send(granted.as_bool());
		});
		unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler) };

		return match rx.recv_timeout(ACCESS_TIMEOUT) {
			Ok(true) => Ok(()),
			Ok(false) => Err(Error::Codec(anyhow::anyhow!(
				"camera access denied; enable it in System Settings > Privacy & Security > Camera"
			))),
			Err(_) => Err(Error::Codec(anyhow::anyhow!(
				"timed out after {ACCESS_TIMEOUT:?} waiting for the camera-permission prompt"
			))),
		};
	}

	// Denied or restricted: no prompt will appear, so fail fast with a fix.
	Err(Error::Codec(anyhow::anyhow!(
		"camera access not authorized (denied or restricted); enable it in System Settings > Privacy & Security > Camera"
	)))
}

pub(super) struct Camera {
	// Kept alive (and running) for the life of the capture; dropped stops it.
	session: Retained<AVCaptureSession>,
	queue: Arc<FrameQueue>,
	_delegate: Retained<Delegate>,
	_dispatch: DispatchRetained<DispatchQueue>,
	width: u32,
	height: u32,
	device: String,
	/// The first frame, captured during `open` to learn the resolution.
	pending: Option<Frame>,
}

impl Camera {
	pub(super) fn open(config: &Config) -> Result<Self, Error> {
		let media = unsafe { AVMediaTypeVideo }.ok_or_else(|| Error::Codec(anyhow::anyhow!("AVMediaTypeVideo")))?;

		// Gate on camera authorization before opening the device, so an
		// unauthorized client gets a clear error (and a prompt on first run)
		// instead of a silent first-frame timeout.
		ensure_camera_access(media)?;

		let device = match &config.device {
			Some(id) => {
				let id = NSString::from_str(id);
				unsafe { AVCaptureDevice::deviceWithUniqueID(&id) }
					.ok_or_else(|| Error::Codec(anyhow::anyhow!("no camera with id {id}")))?
			}
			None => unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media) }
				.ok_or_else(|| Error::Codec(anyhow::anyhow!("no default camera")))?,
		};
		let device_id = unsafe { device.uniqueID() }.to_string();

		let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
			.map_err(|e| Error::Codec(anyhow::anyhow!("camera input: {e:?}")))?;

		let queue = FrameQueue::new();
		let delegate = Delegate::new(queue.clone());
		let dispatch = DispatchQueue::new("dev.moq.video.capture", None);

		let output = unsafe { AVCaptureVideoDataOutput::new() };
		unsafe {
			// Drop late frames instead of queuing them; we want the newest.
			output.setAlwaysDiscardsLateVideoFrames(true);
			let proto = ProtocolObject::from_ref(&*delegate);
			output.setSampleBufferDelegate_queue(Some(proto), Some(&dispatch));
		}

		let session = unsafe { AVCaptureSession::new() };
		unsafe {
			session.beginConfiguration();
			if !session.canAddInput(&input) {
				return Err(Error::Codec(anyhow::anyhow!("cannot add camera input")));
			}
			session.addInput(&input);
			if !session.canAddOutput(&output) {
				return Err(Error::Codec(anyhow::anyhow!("cannot add video output")));
			}
			session.addOutput(&output);
			session.commitConfiguration();
			session.startRunning();
		}

		// Block for the first frame to learn the negotiated resolution (and to
		// surface a permission failure as an error rather than a silent hang).
		let first = queue.pop_timeout(FIRST_FRAME_TIMEOUT).ok_or_else(|| {
			Error::Codec(anyhow::anyhow!(
				"no frames from camera {device_id} within {FIRST_FRAME_TIMEOUT:?} (permission denied?)"
			))
		})?;
		let (width, height) = (first.width(), first.height());

		tracing::info!(device = %device_id, width, height, "opened camera (AVFoundation)");

		Ok(Self {
			session,
			queue,
			_delegate: delegate,
			_dispatch: dispatch,
			width,
			height,
			device: device_id,
			pending: Some(first),
		})
	}
}

impl FrameSource for Camera {
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

	/// AVFoundation doesn't hand us a frame rate up front; let the caller pick
	/// its default.
	fn framerate(&self) -> Option<u32> {
		None
	}

	fn device(&self) -> &str {
		&self.device
	}
}

impl Drop for Camera {
	fn drop(&mut self) {
		unsafe { self.session.stopRunning() };
		self.queue.close();
	}
}

struct DelegateIvars {
	queue: Arc<FrameQueue>,
}

define_class!(
	#[unsafe(super(NSObject))]
	#[name = "MoqVideoCameraDelegate"]
	#[ivars = DelegateIvars]
	struct Delegate;

	unsafe impl NSObjectProtocol for Delegate {}

	unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Delegate {
		#[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
		unsafe fn did_output(
			&self,
			_output: &AVCaptureOutput,
			sample_buffer: &CMSampleBuffer,
			_connection: &AVCaptureConnection,
		) {
			if let Some(frame) = surface_frame(sample_buffer) {
				self.ivars().queue.push(frame);
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
