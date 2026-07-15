//! Camera capture via AVFoundation (macOS), the zero-copy path.
//!
//! `AVCaptureVideoDataOutput` delivers IOSurface-backed `CVPixelBuffer`s on a
//! dispatch queue; the delegate wraps each as a [`Frame::Surface`] and pushes it
//! into the shared [`FrameChannel`], which the encode loop awaits. Frames reach
//! VideoToolbox with no copy and no color conversion.

use std::sync::{Arc, Mutex};
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

use super::surface::surface_frame;
use super::{Camera, Config, FrameChannel, FrameStream};
use crate::Error;

/// How long `open` waits for the first frame before assuming the camera never
/// started (e.g. permission denied).
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the user to answer the camera-permission prompt the
/// first time capture runs.
const ACCESS_TIMEOUT: Duration = Duration::from_secs(60);

/// List the cameras. Unlike capture, enumeration needs no TCC grant: an
/// unauthorized process still sees the devices, just not their frames.
pub(super) fn cameras() -> Result<Vec<Camera>, Error> {
	let media = unsafe { AVMediaTypeVideo }.ok_or_else(|| Error::Codec(anyhow::anyhow!("AVMediaTypeVideo")))?;
	// The suggested replacement, AVCaptureDeviceDiscoverySession, has to be handed
	// an explicit list of device types, and the constants for external and
	// Continuity cameras are macOS 14+. Naming them would drop those cameras on
	// older systems (or miss a symbol); this returns every video device on every
	// version, which is exactly what listing wants.
	#[allow(deprecated)]
	let devices = unsafe { AVCaptureDevice::devicesWithMediaType(media) };
	Ok((0..devices.count())
		.map(|index| devices.objectAtIndex(index))
		.map(|device| Camera {
			id: unsafe { device.uniqueID() }.to_string(),
			name: unsafe { device.localizedName() }.to_string(),
		})
		.collect())
}

/// Open the default (or requested) camera and stream its frames.
pub(super) async fn open(config: &Config, device: Option<&str>) -> Result<FrameStream, Error> {
	let media = unsafe { AVMediaTypeVideo }.ok_or_else(|| Error::Codec(anyhow::anyhow!("AVMediaTypeVideo")))?;

	// Gate on camera authorization before opening the device, so an unauthorized
	// client gets a clear error (and a prompt on first run) instead of a silent
	// first-frame timeout.
	ensure_camera_access(media).await?;

	let device = match device {
		Some(id) => {
			let id = NSString::from_str(id);
			unsafe { AVCaptureDevice::deviceWithUniqueID(&id) }
				.ok_or_else(|| Error::Codec(anyhow::anyhow!("no camera with id {id}")))?
		}
		None => unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media) }
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("no default camera")))?,
	};

	// Honoring these means picking an `activeFormat` and locking a frame
	// duration, which this backend doesn't do yet. Say so rather than letting the
	// camera quietly come up at its default mode.
	if config.width.is_some() || config.height.is_some() || config.framerate.is_some() {
		tracing::warn!("width/height/framerate are ignored for camera capture on macOS; using the device default");
	}
	let device_id = unsafe { device.uniqueID() }.to_string();

	let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }
		.map_err(|e| Error::Codec(anyhow::anyhow!("camera input: {e:?}")))?;

	let chan = FrameChannel::new();
	let delegate = Delegate::new(chan.clone());
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

	// The session keeps capturing until dropped; this guard stops it and closes
	// the channel when the FrameStream goes away.
	let guard = SessionGuard {
		session,
		chan: chan.clone(),
		_delegate: delegate,
		_dispatch: dispatch,
	};

	// Await the first frame to learn the negotiated resolution (and to surface a
	// permission failure as an error rather than a silent hang).
	let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, chan.recv()).await {
		Ok(Some(frame)) => frame,
		Ok(None) | Err(_) => {
			return Err(Error::Codec(anyhow::anyhow!(
				"no frames from camera {device_id} within {FIRST_FRAME_TIMEOUT:?} (permission denied?)"
			)));
		}
	};
	let (width, height) = (first.width(), first.height());

	tracing::info!(device = %device_id, width, height, "opened camera (AVFoundation)");

	Ok(FrameStream::new(
		chan,
		width,
		height,
		// AVFoundation doesn't hand us a frame rate up front; let the caller pick.
		None,
		device_id,
		Some(first),
		Box::new(guard),
	))
}

/// Ensure the process is authorized to use the camera, prompting once if the
/// decision hasn't been made yet.
///
/// macOS otherwise vends black/no frames for an unauthorized client, which
/// surfaces as the confusing [`FIRST_FRAME_TIMEOUT`] hang. Requesting up front
/// turns "denied" into an immediate, actionable error and awaits the system
/// prompt while the user decides. The prompt is attributed to the responsible
/// app (the one that launched the process), so a bare CLI inherits its host
/// app's grant.
async fn ensure_camera_access(media: &AVMediaType) -> Result<(), Error> {
	let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };

	if status == AVAuthorizationStatus::Authorized {
		return Ok(());
	}
	if status == AVAuthorizationStatus::NotDetermined {
		// requestAccess invokes the handler once, asynchronously, on an arbitrary
		// queue. Bridge it to a oneshot we await, so the prompt doesn't block the
		// runtime and a cancelled capture drops cleanly mid-prompt.
		let (tx, rx) = tokio::sync::oneshot::channel();
		let tx = Mutex::new(Some(tx));
		let handler = RcBlock::new(move |granted: Bool| {
			if let Some(tx) = tx.lock().unwrap().take() {
				let _ = tx.send(granted.as_bool());
			}
		});
		unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler) };

		return match tokio::time::timeout(ACCESS_TIMEOUT, rx).await {
			Ok(Ok(true)) => Ok(()),
			Ok(Ok(false)) => Err(Error::Codec(anyhow::anyhow!(
				"camera access denied; enable it in System Settings > Privacy & Security > Camera"
			))),
			Ok(Err(_)) => Err(Error::Codec(anyhow::anyhow!(
				"camera-permission prompt dismissed without a decision"
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

/// Keeps the capture session (and its delegate) alive; stops it on drop, which
/// turns the camera LED off and closes the channel so a parked read returns.
struct SessionGuard {
	session: Retained<AVCaptureSession>,
	chan: Arc<FrameChannel>,
	_delegate: Retained<Delegate>,
	_dispatch: DispatchRetained<DispatchQueue>,
}

impl Drop for SessionGuard {
	fn drop(&mut self) {
		unsafe { self.session.stopRunning() };
		self.chan.close();
	}
}

struct DelegateIvars {
	chan: Arc<FrameChannel>,
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
				self.ivars().chan.push(frame);
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
