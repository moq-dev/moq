//! Shared Media Foundation / COM helpers used by the capture source, the
//! hardware encoder backend, and the hardware decoder backend on Windows.

use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::{
	IMFDXGIDeviceManager, IMFMediaBuffer, MF_VERSION, MFCreateDXGIDeviceManager, MFCreateMemoryBuffer, MFSTARTUP_FULL,
	MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};

use crate::Error;
use crate::frame::d3d11;

/// Wrap a `windows` error with context as a codec error.
pub(crate) fn mf_err(ctx: &str, e: windows::core::Error) -> Error {
	Error::codec(anyhow::anyhow!("{ctx}: {e}"))
}

/// A system-memory buffer of `len` bytes, refusing a length Media Foundation
/// cannot hold rather than truncating it.
pub(crate) fn memory_buffer(len: usize) -> Result<IMFMediaBuffer, Error> {
	let len = u32::try_from(len)
		.map_err(|_| Error::codec(anyhow::anyhow!("{len} bytes exceeds a Media Foundation buffer")))?;
	unsafe { MFCreateMemoryBuffer(len) }.map_err(|e| mf_err("MFCreateMemoryBuffer", e))
}

/// An `IMFMediaBuffer` locked for access, unlocked on drop so no early return
/// leaks the lock.
pub(crate) struct Locked<'a> {
	buffer: &'a IMFMediaBuffer,
	data: *mut u8,
	max: usize,
	current: usize,
}

impl<'a> Locked<'a> {
	pub(crate) fn new(buffer: &'a IMFMediaBuffer) -> Result<Self, Error> {
		let mut data = std::ptr::null_mut();
		let mut max = 0;
		let mut current = 0;
		unsafe { buffer.Lock(&mut data, Some(&mut max), Some(&mut current)) }.map_err(|e| mf_err("lock buffer", e))?;
		let locked = Self {
			buffer,
			data,
			max: max as usize,
			current: current as usize,
		};
		// A null pointer is only an empty buffer; anything else is unreadable.
		if locked.data.is_null() && locked.max != 0 {
			return Err(Error::codec(anyhow::anyhow!(
				"Media Foundation locked a {max} byte buffer at a null address"
			)));
		}
		Ok(locked)
	}

	/// The valid bytes, up to the buffer's current length.
	pub(crate) fn data(&self) -> &[u8] {
		if self.data.is_null() {
			return &[];
		}
		// SAFETY: the lock maps `max` bytes at `data` until it is released, and
		// Media Foundation never reports a current length past the maximum.
		unsafe { std::slice::from_raw_parts(self.data, self.current.min(self.max)) }
	}

	/// The first `len` bytes to write, refused if the buffer holds fewer.
	pub(crate) fn data_mut(&mut self, len: usize) -> Result<&mut [u8], Error> {
		if len > self.max {
			return Err(Error::codec(anyhow::anyhow!(
				"Media Foundation buffer holds {} bytes, not {len}",
				self.max
			)));
		}
		if len == 0 {
			return Ok(&mut []);
		}
		// SAFETY: as `data`, `len` is within the mapped `max` bytes, and `&mut
		// self` excludes any other view of them.
		Ok(unsafe { std::slice::from_raw_parts_mut(self.data, len) })
	}
}

impl Drop for Locked<'_> {
	fn drop(&mut self) {
		let _ = unsafe { self.buffer.Unlock() };
	}
}

/// Pack two 32-bit values into the high/low halves of a u64, the layout Media
/// Foundation uses for `MF_MT_FRAME_SIZE` (width/height) and `MF_MT_FRAME_RATE`
/// (numerator/denominator).
pub(crate) fn pack_2x32(hi: u32, lo: u32) -> u64 {
	((hi as u64) << 32) | lo as u64
}

/// Split the high/low halves of a `pack_2x32` value back out, the inverse of
/// [`pack_2x32`]. Used to read `MF_MT_FRAME_SIZE` / `MF_MT_FRAME_RATE` back off a
/// negotiated media type.
pub(crate) fn unpack_2x32(v: u64) -> (u32, u32) {
	((v >> 32) as u32, v as u32)
}

/// Create a hardware Direct3D11 device plus a DXGI device manager wrapping it,
/// the pairing every Media Foundation GPU path needs (a capture source reader, a
/// hardware encoder MFT, or a hardware decoder MFT). The device comes from the
/// shared [`d3d11::create_device`] (multithread-protected); this adds the Media
/// Foundation manager on top.
pub(crate) fn create_d3d_device() -> Result<(ID3D11Device, IMFDXGIDeviceManager), Error> {
	let device = d3d11::create_device(None)?;

	let mut token: u32 = 0;
	let mut manager: Option<IMFDXGIDeviceManager> = None;
	unsafe {
		MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(|e| mf_err("MFCreateDXGIDeviceManager", e))?;
	}
	let manager = manager.ok_or_else(|| Error::codec(anyhow::anyhow!("MFCreateDXGIDeviceManager returned null")))?;
	unsafe {
		manager
			.ResetDevice(&device, token)
			.map_err(|e| mf_err("ResetDevice", e))?;
	}

	Ok((device, manager))
}

/// COM (MTA) + Media Foundation lifetime for the calling thread, balanced by
/// `MFShutdown` + `CoUninitialize` on drop. Both calls are refcounted, so a
/// capture source and an encoder backend can each hold one on the same blocking
/// thread without stepping on each other.
pub(crate) struct ComGuard {
	started: bool,
}

impl ComGuard {
	pub(crate) fn new() -> Result<Self, Error> {
		unsafe {
			// MTA: the blocking capture thread has no message pump. `S_FALSE`
			// (already initialized on this thread) is success, which `.ok()` keeps.
			CoInitializeEx(None, COINIT_MULTITHREADED)
				.ok()
				.map_err(|e| mf_err("CoInitializeEx", e))?;
			let mut guard = Self { started: false };
			MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| mf_err("MFStartup", e))?;
			guard.started = true;
			Ok(guard)
		}
	}
}

impl Drop for ComGuard {
	fn drop(&mut self) {
		unsafe {
			if self.started {
				let _ = MFShutdown();
			}
			CoUninitialize();
		}
	}
}
