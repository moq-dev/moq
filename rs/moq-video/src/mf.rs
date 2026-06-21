//! Shared Media Foundation / COM helpers used by the capture source, the
//! hardware encoder backend, and the hardware decoder backend on Windows.

use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D10::ID3D10Multithread;
use windows::Win32::Graphics::Direct3D11::{
	D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
	ID3D11Device,
};
use windows::Win32::Media::MediaFoundation::{
	IMFDXGIDeviceManager, MF_VERSION, MFCreateDXGIDeviceManager, MFSTARTUP_FULL, MFShutdown, MFStartup,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::core::Interface;

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

/// Split the high/low halves of a `pack_2x32` value back out, the inverse of
/// [`pack_2x32`]. Used to read `MF_MT_FRAME_SIZE` / `MF_MT_FRAME_RATE` back off a
/// negotiated media type.
pub(crate) fn unpack_2x32(v: u64) -> (u32, u32) {
	((v >> 32) as u32, v as u32)
}

/// Create a hardware Direct3D11 device plus a DXGI device manager wrapping it,
/// the pairing every Media Foundation GPU path needs (a capture source reader, a
/// hardware encoder MFT, or a hardware decoder MFT). The device is marked
/// multithread-protected because Media Foundation's internal worker threads and
/// our blocking thread both touch it.
pub(crate) fn create_d3d_device() -> Result<(ID3D11Device, IMFDXGIDeviceManager), Error> {
	let mut device: Option<ID3D11Device> = None;
	unsafe {
		D3D11CreateDevice(
			None,
			D3D_DRIVER_TYPE_HARDWARE,
			HMODULE::default(),
			D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
			None,
			D3D11_SDK_VERSION,
			Some(&mut device),
			None,
			None,
		)
		.map_err(|e| mf_err("D3D11CreateDevice", e))?;
	}
	let device = device.ok_or_else(|| Error::Codec(anyhow::anyhow!("D3D11CreateDevice returned null")))?;

	let multithread = device
		.cast::<ID3D10Multithread>()
		.map_err(|e| mf_err("query ID3D10Multithread", e))?;
	unsafe {
		let _ = multithread.SetMultithreadProtected(true);
	}

	let mut token: u32 = 0;
	let mut manager: Option<IMFDXGIDeviceManager> = None;
	unsafe {
		MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(|e| mf_err("MFCreateDXGIDeviceManager", e))?;
	}
	let manager = manager.ok_or_else(|| Error::Codec(anyhow::anyhow!("MFCreateDXGIDeviceManager returned null")))?;
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
