//! Shared Windows COM + Media Foundation init, used by both the capture source
//! ([`capture::mediafoundation`](crate::capture)) and the hardware encoder
//! backend ([`encode::backend::mediafoundation`](crate::encode)).

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::Error;

/// Wrap a Media Foundation `windows::core::Error` with context as a codec error.
pub(crate) fn mf_err(ctx: &str, e: windows::core::Error) -> Error {
	Error::Codec(anyhow::anyhow!("{ctx}: {e}"))
}

/// Initialize COM (MTA) + Media Foundation for the calling thread, balanced by
/// `MFShutdown` + `CoUninitialize` on drop. Both are refcounted, so nesting a
/// guard (capture + encoder on the same blocking thread) is fine: each `new`
/// bumps the count and each drop releases it.
pub(crate) struct ComGuard;

impl ComGuard {
	pub(crate) fn new() -> Result<Self, Error> {
		unsafe {
			// MTA: the blocking capture/encode thread has no message pump. `S_FALSE`
			// (already initialized on this thread) is success, which `.ok()` keeps.
			CoInitializeEx(None, COINIT_MULTITHREADED)
				.ok()
				.map_err(|e| mf_err("CoInitializeEx", e))?;
			MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| mf_err("MFStartup", e))?;
		}
		Ok(Self)
	}
}

impl Drop for ComGuard {
	fn drop(&mut self) {
		unsafe {
			let _ = MFShutdown();
			CoUninitialize();
		}
	}
}
