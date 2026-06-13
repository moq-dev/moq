//! Microphone permission pre-check.
//!
//! cpal can't see macOS TCC, so a denied mic just yields silence (or no
//! callbacks at all) and the capture loop would otherwise hang until the
//! first-buffer timeout. Querying AVFoundation lets us fail fast with a precise
//! error, and trigger the system prompt when access hasn't been decided yet.
//!
//! On every other platform this is a no-op: permission, if any, is enforced by
//! the OS audio stack and surfaces through cpal or the timeout.

use crate::AudioError;

#[cfg(target_os = "macos")]
pub(super) fn ensure_microphone_access() -> Result<(), AudioError> {
	use objc2_av_foundation::{AVAuthorizationStatus, AVCaptureDevice, AVMediaTypeAudio};

	let media =
		unsafe { AVMediaTypeAudio }.ok_or_else(|| AudioError::Unsupported("AVMediaTypeAudio unavailable".into()))?;

	let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };

	if status == AVAuthorizationStatus::Authorized {
		return Ok(());
	}
	if status == AVAuthorizationStatus::Denied {
		return Err(denied());
	}
	if status == AVAuthorizationStatus::Restricted {
		return Err(AudioError::Unsupported(
			"microphone access is restricted by system policy (parental controls / MDM)".into(),
		));
	}
	if status == AVAuthorizationStatus::NotDetermined {
		return request_access(media);
	}

	// Unknown future status: don't block capture, let the stream open and the
	// first-buffer timeout catch a genuine hang.
	Ok(())
}

/// Trigger the system prompt and block (on this `spawn_blocking` thread) until
/// the user answers. Unbundled CLIs usually get auto-denied without UI, which we
/// surface as the same clear error.
#[cfg(target_os = "macos")]
fn request_access(media: &objc2_av_foundation::AVMediaType) -> Result<(), AudioError> {
	use objc2_av_foundation::AVCaptureDevice;

	let (tx, rx) = std::sync::mpsc::channel::<bool>();
	let handler = block2::RcBlock::new(move |granted: objc2::runtime::Bool| {
		let _ = tx.send(granted.as_bool());
	});

	unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &handler) };

	match rx.recv() {
		Ok(true) => Ok(()),
		Ok(false) => Err(denied()),
		// The completion handler was dropped without firing; fall through to the
		// stream + timeout rather than hard-failing.
		Err(_) => Ok(()),
	}
}

#[cfg(target_os = "macos")]
fn denied() -> AudioError {
	AudioError::Unsupported(
		"microphone access denied; grant it in System Settings > Privacy & Security > Microphone".into(),
	)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn ensure_microphone_access() -> Result<(), AudioError> {
	Ok(())
}
