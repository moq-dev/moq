//! Shared Media Foundation / COM helpers used by both the capture source and the
//! hardware encoder backend on Windows.

use windows::Win32::Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::Error;

/// Wrap a `windows` error with context as a codec error.
pub(crate) fn mf_err(ctx: &str, e: windows::core::Error) -> Error {
	Error::Codec(anyhow::anyhow!("{ctx}: {e}"))
}

/// Pack two 32-bit values into the high/low halves of a u64, the layout Media
/// Foundation uses for `MF_MT_FRAME_SIZE` (width/height) and `MF_MT_FRAME_RATE`
/// (numerator/denominator).
pub(crate) fn pack_2x32(hi: u32, lo: u32) -> u64 {
	((hi as u64) << 32) | lo as u64
}

/// COM (MTA) + Media Foundation lifetime for the calling thread, balanced by
/// `MFShutdown` + `CoUninitialize` on drop. Both calls are refcounted, so a
/// capture source and an encoder backend can each hold one on the same blocking
/// thread without stepping on each other.
pub(crate) struct ComGuard;

impl ComGuard {
	pub(crate) fn new() -> Result<Self, Error> {
		unsafe {
			// MTA: the blocking capture thread has no message pump. `S_FALSE`
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
