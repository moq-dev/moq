//! Camera capture via AVFoundation (macOS), the zero-copy path.
//!
//! `AVCaptureVideoDataOutput` delivers IOSurface-backed `CVPixelBuffer`s on a
//! dispatch queue; the delegate wraps each as a [`Surface::PixelBuffer`] and pushes it
//! into the shared [`FrameChannel`], which the encode loop awaits. Frames reach
//! VideoToolbox with no copy and no color conversion.
//!
//! Sample delivery is the only callback AVFoundation makes on the happy path, so
//! the same delegate also observes the notifications that say the session died:
//! a camera that vanishes simply stops delivering, and without them a parked
//! [`Stream::read`] would never return. See [`observe`] for which notifications
//! are terminal and why an interruption is not.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send, sel};
use objc2_av_foundation::{
	AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput,
	AVCaptureDeviceWasDisconnectedNotification, AVCaptureOutput, AVCaptureSession, AVCaptureSessionErrorKey,
	AVCaptureSessionInterruptionEndedNotification, AVCaptureSessionRuntimeErrorNotification,
	AVCaptureSessionWasInterruptedNotification, AVCaptureVideoDataOutput, AVCaptureVideoDataOutputSampleBufferDelegate,
	AVError, AVFoundationErrorDomain, AVMediaType, AVMediaTypeVideo,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSError, NSNotification, NSNotificationCenter, NSObject, NSObjectProtocol, NSString};

use super::surface::surface_frame;
use super::{Camera, Config, FrameChannel, Stream};
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
pub(super) async fn open(config: &Config, device: Option<&str>) -> Result<Stream, Error> {
	let media = unsafe { AVMediaTypeVideo }.ok_or_else(|| Error::Codec(anyhow::anyhow!("AVMediaTypeVideo")))?;

	// Gate on camera authorization before opening the device, so an unauthorized
	// client gets a clear error (and a prompt on first run) instead of a silent
	// first-frame timeout.
	ensure_camera_access(media).await?;

	let device = match device {
		Some(id) => {
			let id = NSString::from_str(id);
			unsafe { AVCaptureDevice::deviceWithUniqueID(&id) }
				.ok_or_else(|| Error::SourceUnavailable(format!("no camera with id {id}")))?
		}
		None => unsafe { AVCaptureDevice::defaultDeviceWithMediaType(media) }
			.ok_or_else(|| Error::SourceUnavailable("no default camera".to_string()))?,
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
	let delegate = Delegate::new(chan.clone(), device_id.clone());
	let dispatch = DispatchQueue::new("dev.moq.video.capture", None);

	let output = unsafe { AVCaptureVideoDataOutput::new() };
	unsafe {
		// Drop late frames instead of queuing them; we want the newest.
		output.setAlwaysDiscardsLateVideoFrames(true);
		let proto = ProtocolObject::from_ref(&*delegate);
		output.setSampleBufferDelegate_queue(Some(proto), Some(&dispatch));
	}

	let session = unsafe { AVCaptureSession::new() };

	// Own the session (and register the failure observers) before configuring it,
	// so an early return here still unregisters and releases everything. It also
	// means a session that fails during `startRunning` is observed: that failure
	// arrives as a runtime-error notification, which an observer added afterwards
	// would miss.
	let guard = SessionGuard::new(session.clone(), delegate, dispatch);

	let configuration = SessionConfiguration::new(&session);
	unsafe {
		if !session.canAddInput(&input) {
			return Err(Error::Codec(anyhow::anyhow!("cannot add camera input")));
		}
		session.addInput(&input);
		if !session.canAddOutput(&output) {
			return Err(Error::Codec(anyhow::anyhow!("cannot add video output")));
		}
		session.addOutput(&output);
	}
	drop(configuration);
	unsafe { session.startRunning() };

	// Await the first frame to learn the negotiated resolution (and to surface a
	// permission failure as an error rather than a silent hang).
	let first = match tokio::time::timeout(FIRST_FRAME_TIMEOUT, chan.recv()).await {
		Ok(Ok(Some(frame))) => frame,
		Ok(Err(error)) => return Err(error),
		Ok(Ok(None)) | Err(_) => {
			return Err(Error::Codec(anyhow::anyhow!(
				"no frames from camera {device_id} within {FIRST_FRAME_TIMEOUT:?} (permission denied?)"
			)));
		}
	};
	let (width, height) = (first.width(), first.height());

	tracing::info!(device = %device_id, width, height, "opened camera (AVFoundation)");

	Ok(Stream::new(
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
			Ok(Ok(false)) => Err(Error::PermissionDenied(
				"camera access; enable it in System Settings > Privacy & Security > Camera".to_string(),
			)),
			Ok(Err(_)) => Err(Error::PermissionDenied(
				"camera-permission prompt dismissed without a decision".to_string(),
			)),
			Err(_) => Err(Error::PermissionDenied(format!(
				"timed out after {ACCESS_TIMEOUT:?} waiting for the camera-permission prompt"
			))),
		};
	}

	// Denied or restricted: no prompt will appear, so fail fast with a fix.
	Err(Error::PermissionDenied(
		"camera access is denied or restricted; enable it in System Settings > Privacy & Security > Camera".to_string(),
	))
}

/// Subscribe `delegate` to the notifications that mean the camera stopped
/// producing frames for good.
///
/// AVFoundation has no per-frame error path: the only thing it calls on a
/// healthy session is the sample-buffer delegate, so a camera that is unplugged
/// or revoked just goes quiet. These notifications are the whole signal.
///
/// - **Runtime error** ends the session; [`runtime_error`] sorts a revoked grant
///   from a vanished device. Terminal.
/// - **Device disconnected** is the unplug, which is terminal even when the
///   session hasn't noticed yet.
/// - **Interruption is not terminal.** AVFoundation stops an interrupted session
///   and restarts it itself once the interruption ends, so frames resume and the
///   parked read simply wakes with the next one; failing here would tear down a
///   publish the OS is about to resume. macOS also gives us no way to tell a
///   recoverable interruption from a hopeless one, because
///   `AVCaptureSessionInterruptionReasonKey` is iOS-only, so guessing "terminal"
///   would be guessing. The causes that really are fatal (unplug, revoked
///   permission, a session-killing error) each post their own notification
///   above, so nothing is lost by letting an interruption ride.
fn observe(delegate: &Delegate, session: &AVCaptureSession) {
	let center = NSNotificationCenter::defaultCenter();
	let observer: &AnyObject = delegate;
	let session: &AnyObject = session;

	// SAFETY: the observer outlives the registration; `SessionGuard::drop`
	// removes it before releasing the delegate, and every selector below is
	// defined on that class taking the notification.
	unsafe {
		center.addObserver_selector_name_object(
			observer,
			sel!(moqSessionRuntimeError:),
			Some(AVCaptureSessionRuntimeErrorNotification),
			Some(session),
		);
		center.addObserver_selector_name_object(
			observer,
			sel!(moqSessionWasInterrupted:),
			Some(AVCaptureSessionWasInterruptedNotification),
			Some(session),
		);
		center.addObserver_selector_name_object(
			observer,
			sel!(moqSessionInterruptionEnded:),
			Some(AVCaptureSessionInterruptionEndedNotification),
			Some(session),
		);
		// Matched by `uniqueID` in the handler rather than by filtering on our
		// `AVCaptureDevice` here: the notification carries whichever instance
		// AVFoundation holds for the camera, and filtering on ours would silently
		// never fire if the two ever differ.
		center.addObserver_selector_name_object(
			observer,
			sel!(moqDeviceWasDisconnected:),
			Some(AVCaptureDeviceWasDisconnectedNotification),
			None,
		);
	}
}

/// The `NSError` an `AVCaptureSessionRuntimeErrorNotification` carries under
/// `AVCaptureSessionErrorKey`.
fn notification_error(notification: &NSNotification) -> Option<Retained<NSError>> {
	let info = notification.userInfo()?;
	let key: &AnyObject = unsafe { AVCaptureSessionErrorKey };
	info.objectForKey(key)?.downcast::<NSError>().ok()
}

/// Translate a session runtime error into the error the consumer sees.
///
/// A revoked camera grant and a camera that went away both arrive as this one
/// notification, so the `AVError` code is what separates "you may not" from
/// "it's gone". Every other cause still stopped the session, so it is terminal
/// too and reports the system's own description.
fn runtime_error(error: Option<&NSError>) -> Error {
	let Some(error) = error else {
		return Error::SourceUnavailable("camera session stopped without a reason".to_string());
	};

	let reason = error.localizedDescription().to_string();
	let domain = error.domain();
	let av = unsafe { AVFoundationErrorDomain }.is_some_and(|expected| *domain == *expected);
	let code = AVError(error.code());

	if av && (code == AVError::ApplicationIsNotAuthorizedToUseDevice || code == AVError::ApplicationIsNotAuthorized) {
		return Error::PermissionDenied(reason);
	}

	Error::SourceUnavailable(reason)
}

/// Commits the session configuration before any later guard stops the session.
///
/// `stopRunning` raises an Objective-C exception while a configuration is open,
/// so this must also cover the early error returns from `open`.
struct SessionConfiguration<'a> {
	session: &'a AVCaptureSession,
}

impl<'a> SessionConfiguration<'a> {
	fn new(session: &'a AVCaptureSession) -> Self {
		unsafe { session.beginConfiguration() };
		Self { session }
	}
}

impl Drop for SessionConfiguration<'_> {
	fn drop(&mut self) {
		unsafe { self.session.commitConfiguration() };
	}
}

/// Keeps the capture session (and its delegate) alive; stops it on drop, which
/// turns the camera LED off and closes the channel so a parked read returns.
struct SessionGuard {
	session: Retained<AVCaptureSession>,
	chan: Arc<FrameChannel>,
	delegate: Retained<Delegate>,
	_dispatch: DispatchRetained<DispatchQueue>,
}

impl SessionGuard {
	/// Take ownership of a session and subscribe its delegate to the failure
	/// notifications, which the guard unsubscribes on drop.
	fn new(
		session: Retained<AVCaptureSession>,
		delegate: Retained<Delegate>,
		dispatch: DispatchRetained<DispatchQueue>,
	) -> Self {
		observe(&delegate, &session);
		let chan = delegate.ivars().chan.clone();
		Self {
			session,
			chan,
			delegate,
			_dispatch: dispatch,
		}
	}
}

impl Drop for SessionGuard {
	fn drop(&mut self) {
		// Unregister first. NSNotificationCenter does not retain its observers, so
		// a notification posted after the delegate is released would message freed
		// memory.
		// SAFETY: `delegate` is the object `observe` registered.
		unsafe { NSNotificationCenter::defaultCenter().removeObserver(&self.delegate) };
		unsafe { self.session.stopRunning() };
		self.chan.close();
	}
}

struct DelegateIvars {
	chan: Arc<FrameChannel>,
	/// The `uniqueID` of the camera being captured, to match disconnect
	/// notifications against.
	device_id: String,
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

	// The notification handlers registered by `observe`. Prefixed so they can't
	// collide with anything AVFoundation declares on our superclass.
	impl Delegate {
		#[unsafe(method(moqSessionRuntimeError:))]
		fn session_runtime_error(&self, notification: &NSNotification) {
			let error = runtime_error(notification_error(notification).as_deref());
			tracing::warn!(device = %self.ivars().device_id, %error, "camera session failed");
			self.ivars().chan.fail(error);
		}

		#[unsafe(method(moqDeviceWasDisconnected:))]
		fn device_was_disconnected(&self, notification: &NSNotification) {
			let Some(object) = notification.object() else { return };
			// SAFETY: this notification's object is always the AVCaptureDevice that
			// went away. A checked `downcast` is not an option: AVFoundation's private
			// device subclasses declare `isKindOfClass:` with a malformed type
			// encoding, which objc2's debug-build message verification aborts on.
			let device: Retained<AVCaptureDevice> = unsafe { Retained::cast_unchecked(object) };

			let id = unsafe { device.uniqueID() }.to_string();
			if id != self.ivars().device_id {
				return; // some other camera came or went
			}

			tracing::warn!(device = %id, "camera was disconnected");
			self.ivars().chan.fail(Error::SourceUnavailable(format!("camera {id} was disconnected")));
		}

		#[unsafe(method(moqSessionWasInterrupted:))]
		fn session_was_interrupted(&self, _notification: &NSNotification) {
			// Recoverable by design; see `observe`. Frames stop until the
			// interruption ends, which the reader sees as a gap, not an error.
			tracing::warn!(device = %self.ivars().device_id, "camera session interrupted; waiting for it to resume");
		}

		#[unsafe(method(moqSessionInterruptionEnded:))]
		fn session_interruption_ended(&self, _notification: &NSNotification) {
			tracing::info!(device = %self.ivars().device_id, "camera session resumed");
		}
	}
);

impl Delegate {
	fn new(chan: Arc<FrameChannel>, device_id: String) -> Retained<Self> {
		let this = Self::alloc().set_ivars(DelegateIvars { chan, device_id });
		unsafe { msg_send![super(this), init] }
	}
}

/// The notification observers are the whole fix, and the only way to trigger
/// them for real is to unplug a camera. `NSNotificationCenter` is the injection
/// point AVFoundation already uses, so these tests post the same notifications
/// AVFoundation would and assert what the channel does, without a device, a TCC
/// grant, or a test-only hook in the capture path.
#[cfg(test)]
mod tests {
	use objc2_foundation::{NSDictionary, NSInteger};

	use super::*;

	/// A delegate wired up exactly as `open` wires it, minus the camera: the
	/// session is never configured or started, so nothing lights up.
	fn wire(device_id: &str) -> (Arc<FrameChannel>, Retained<AVCaptureSession>, SessionGuard) {
		let chan = FrameChannel::new();
		let session = unsafe { AVCaptureSession::new() };
		let delegate = Delegate::new(chan.clone(), device_id.to_string());
		let dispatch = DispatchQueue::new("dev.moq.video.capture.test", None);
		let guard = SessionGuard::new(session.clone(), delegate, dispatch);
		(chan, session, guard)
	}

	/// Teardown must never call `stopRunning` while configuration is open. This
	/// is the setup-failure path when `canAddInput` or `canAddOutput` returns false.
	#[test]
	fn an_early_configuration_error_commits_before_teardown() {
		let session = unsafe { AVCaptureSession::new() };
		let configuration = SessionConfiguration::new(&session);
		drop(configuration);

		// Raises `NSGenericException` if the configuration was not committed.
		unsafe { session.stopRunning() };
	}

	/// Post an `AVCaptureSessionRuntimeErrorNotification` for `session` carrying
	/// an `AVFoundationErrorDomain` error with `code`, the way a session that dies
	/// mid-capture does.
	fn post_runtime_error(session: &AVCaptureSession, code: NSInteger) {
		let domain = unsafe { AVFoundationErrorDomain }.expect("AVFoundationErrorDomain");
		let error = unsafe { NSError::errorWithDomain_code_userInfo(domain, code, None) };

		let key: &NSString = unsafe { AVCaptureSessionErrorKey };
		let info = NSDictionary::<NSString, NSError>::from_slices(&[key], &[&*error]);
		// SAFETY: NSDictionary's type parameters are phantom, so erasing them
		// changes nothing about the object; the posting API takes the untyped form.
		let info: Retained<NSDictionary> = unsafe { Retained::cast_unchecked(info) };

		let object: &AnyObject = session;
		unsafe {
			NSNotificationCenter::defaultCenter().postNotificationName_object_userInfo(
				AVCaptureSessionRuntimeErrorNotification,
				Some(object),
				Some(&info),
			);
		}
	}

	/// Post `name` for `session` with no user info.
	fn post(name: &NSString, session: &AVCaptureSession) {
		let object: &AnyObject = session;
		unsafe { NSNotificationCenter::defaultCenter().postNotificationName_object(name, Some(object)) };
	}

	/// The regression: a session that dies has to reach a parked reader. Without
	/// the observers this `recv` never returns.
	#[tokio::test]
	async fn a_runtime_error_ends_the_stream() {
		let (chan, session, _guard) = wire("runtime-error");
		post_runtime_error(&session, AVError::DeviceWasDisconnected.0);

		assert!(matches!(chan.recv().await, Err(Error::SourceUnavailable(_))));
	}

	/// A grant revoked mid-capture arrives as the same notification, so the code
	/// is what has to separate it from a device that vanished.
	#[tokio::test]
	async fn a_revoked_grant_ends_the_stream_as_a_permission_error() {
		let (chan, session, _guard) = wire("revoked-grant");
		post_runtime_error(&session, AVError::ApplicationIsNotAuthorizedToUseDevice.0);

		assert!(matches!(chan.recv().await, Err(Error::PermissionDenied(_))));
	}

	/// A runtime error with no attached cause is still terminal.
	#[test]
	fn a_runtime_error_without_a_cause_is_still_terminal() {
		assert!(matches!(runtime_error(None), Error::SourceUnavailable(_)));
	}

	/// Interruptions are recoverable, so they must leave the channel alone: the
	/// next frame still arrives.
	#[tokio::test]
	async fn an_interruption_does_not_end_the_stream() {
		let (chan, session, _guard) = wire("interrupted");
		post(unsafe { AVCaptureSessionWasInterruptedNotification }, &session);
		post(unsafe { AVCaptureSessionInterruptionEndedNotification }, &session);

		chan.push(crate::frame::Surface::I420(crate::frame::I420 {
			width: 16,
			height: 16,
			data: Vec::new(),
			color: None,
		}));
		assert_eq!(chan.recv().await.unwrap().unwrap().width(), 16);
	}

	/// Unplugging the camera. Needs a real `AVCaptureDevice` to name in the
	/// notification (enumeration needs no TCC grant), so it skips on a machine
	/// with no camera at all.
	#[tokio::test]
	async fn disconnecting_the_captured_camera_ends_the_stream() {
		let Some(camera) = cameras().expect("list cameras").into_iter().next() else {
			return; // headless machine
		};
		let id = NSString::from_str(&camera.id);
		let device = unsafe { AVCaptureDevice::deviceWithUniqueID(&id) }.expect("camera by id");

		let (chan, _session, _guard) = wire(&camera.id);
		let object: &AnyObject = &device;
		unsafe {
			NSNotificationCenter::defaultCenter()
				.postNotificationName_object(AVCaptureDeviceWasDisconnectedNotification, Some(object));
		}

		assert!(matches!(chan.recv().await, Err(Error::SourceUnavailable(_))));
	}

	/// ...but another camera coming or going is not our problem.
	#[tokio::test]
	async fn disconnecting_another_camera_leaves_the_stream_alone() {
		let Some(camera) = cameras().expect("list cameras").into_iter().next() else {
			return; // headless machine
		};
		let id = NSString::from_str(&camera.id);
		let device = unsafe { AVCaptureDevice::deviceWithUniqueID(&id) }.expect("camera by id");

		let (chan, _session, _guard) = wire("some-other-camera");
		let object: &AnyObject = &device;
		unsafe {
			NSNotificationCenter::defaultCenter()
				.postNotificationName_object(AVCaptureDeviceWasDisconnectedNotification, Some(object));
		}

		chan.push(crate::frame::Surface::I420(crate::frame::I420 {
			width: 32,
			height: 32,
			data: Vec::new(),
			color: None,
		}));
		assert_eq!(chan.recv().await.unwrap().unwrap().width(), 32);
	}
}
