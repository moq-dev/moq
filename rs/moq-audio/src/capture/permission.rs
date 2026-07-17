//! Microphone permission pre-check.
//!
//! cpal can't see macOS TCC, so a denied mic just yields silence (or no
//! callbacks at all) and the capture loop would otherwise hang until the
//! first-buffer timeout. Querying AVFoundation lets us fail fast with a precise
//! error, and trigger the system prompt when access hasn't been decided yet.
//!
//! On every other platform this is a no-op: permission, if any, is enforced by
//! the OS audio stack and surfaces through cpal or the timeout.

use crate::Error;

#[cfg(target_os = "macos")]
pub(super) async fn ensure_microphone_access() -> Result<(), Error> {
	use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

	let media = unsafe { AVMediaTypeAudio }.ok_or_else(|| Error::Capture("AVMediaTypeAudio unavailable".into()))?;

	let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };

	if status == AVAuthorizationStatus::Authorized {
		return Ok(());
	}
	if status == AVAuthorizationStatus::Denied {
		return Err(denied());
	}
	if status == AVAuthorizationStatus::Restricted {
		return Err(Error::Capture(
			"microphone access is restricted by system policy (parental controls / MDM)".into(),
		));
	}
	if status == AVAuthorizationStatus::NotDetermined {
		return request_access(media).await;
	}

	// Unknown future status: don't block capture, let the stream open and the
	// first-buffer timeout catch a genuine hang.
	Ok(())
}

/// How long to wait for the user to answer the permission prompt before giving
/// up. Generous, since the dialog blocks on a human, but bounded so a callback
/// that never fires can't hang capture forever (the unbundled-CLI path answers
/// near-instantly). On expiry we fall through to the stream open, where the
/// first-buffer timeout becomes the final backstop.
#[cfg(target_os = "macos")]
const PROMPT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Trigger the system prompt and await the user's answer. Unbundled CLIs usually
/// get auto-denied without UI, which we surface as the same clear error.
#[cfg(target_os = "macos")]
async fn request_access(media: &objc2_av_foundation::AVMediaType) -> Result<(), Error> {
	use std::sync::Mutex;

	use objc2_av_foundation::AVCaptureDevice;

	// requestAccess invokes the handler once, asynchronously, on an arbitrary
	// queue. Bridge it to a oneshot we await, so the prompt doesn't block a
	// runtime worker and a cancelled capture drops cleanly mid-prompt.
	let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
	let tx = Mutex::new(Some(tx));
	let handler = block2::RcBlock::new(move |granted: objc2::runtime::Bool| {
		if let Some(tx) = tx.lock().unwrap().take() {
			let _ = tx.send(granted.as_bool());
		}
	});

	unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler) };

	match tokio::time::timeout(PROMPT_TIMEOUT, rx).await {
		Ok(Ok(true)) => Ok(()),
		Ok(Ok(false)) => Err(denied()),
		// Prompt dismissed without a decision, or the callback never fired within
		// the window: don't hard-fail, fall through to the stream open and let the
		// first-buffer timeout catch a real hang.
		Ok(Err(_)) | Err(_) => Ok(()),
	}
}

#[cfg(target_os = "macos")]
fn denied() -> Error {
	Error::Capture("microphone access denied; grant it in System Settings > Privacy & Security > Microphone".into())
}

#[cfg(not(target_os = "macos"))]
pub(super) async fn ensure_microphone_access() -> Result<(), Error> {
	Ok(())
}
