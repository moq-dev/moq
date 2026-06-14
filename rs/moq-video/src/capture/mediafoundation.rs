//! Native Windows webcam capture via Media Foundation.
//!
//! Drives an [`IMFSourceReader`] over the selected capture device with the source
//! reader's video processor enabled, so whatever the camera emits (MJPEG / YUY2 /
//! NV12) is converted to NV12 for us. Each sample is copied to a tightly packed
//! CPU [`I420`] for the encoder. This is the CPU path feeding openh264 / NVENC;
//! there's no GPU surface here.

use std::ffi::c_void;
use std::ptr;
use std::slice;

use windows::Win32::Media::MediaFoundation::{
	IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaSource, IMFSample, IMFSourceReader,
	MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
	MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
	MF_MT_SUBTYPE, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, MF_SOURCE_READER_FIRST_VIDEO_STREAM,
	MF_SOURCE_READERF_ENDOFSTREAM, MF_VERSION, MFCreateAttributes, MFCreateMediaType,
	MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video, MFSTARTUP_FULL, MFShutdown, MFStartup,
	MFVideoFormat_NV12,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize};
use windows::core::{Interface, PWSTR};

use super::{Config, FrameSource};
use crate::Error;
use crate::frame::{Frame, I420};

fn mf_err(ctx: &str, e: windows::core::Error) -> Error {
	Error::Codec(anyhow::anyhow!("{ctx}: {e}"))
}

/// Pack two 32-bit values into the high/low halves of a u64, the layout Media
/// Foundation uses for `MF_MT_FRAME_SIZE` (width/height) and `MF_MT_FRAME_RATE`
/// (numerator/denominator).
fn pack_2x32(hi: u32, lo: u32) -> u64 {
	((hi as u64) << 32) | lo as u64
}

fn unpack_2x32(v: u64) -> (u32, u32) {
	((v >> 32) as u32, v as u32)
}

/// Initialize COM (MTA) + Media Foundation for the calling thread, balanced by
/// `MFShutdown` + `CoUninitialize` on drop. The capture loop runs on one blocking
/// thread, so each open/close pair stays on the same thread.
struct ComGuard;

impl ComGuard {
	fn new() -> Result<Self, Error> {
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

/// An open camera, read frame-by-frame via [`read`](FrameSource::read).
pub(crate) struct Camera {
	source: IMFMediaSource,
	reader: IMFSourceReader,
	width: u32,
	height: u32,
	framerate: Option<u32>,
	device: String,
	// Drop last: tear down Media Foundation only after the reader/source release.
	_com: ComGuard,
}

impl Camera {
	pub(crate) fn open(config: &Config) -> Result<Self, Error> {
		let com = ComGuard::new()?;
		let (source, device) = open_source(config)?;

		let reader_attrs = create_attributes(1)?;
		unsafe {
			// Insert the video processor so it converts the camera's native
			// format (MJPEG / YUY2 / ...) to the NV12 we request below.
			reader_attrs
				.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
				.map_err(|e| mf_err("enable video processing", e))?;
		}
		let reader = unsafe {
			MFCreateSourceReaderFromMediaSource(&source, &reader_attrs)
				.map_err(|e| mf_err("create source reader", e))?
		};

		// Ask for NV12 at the requested geometry; the reader substitutes the
		// nearest mode it can produce.
		let want = unsafe { MFCreateMediaType().map_err(|e| mf_err("create media type", e))? };
		unsafe {
			want.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
				.map_err(|e| mf_err("set major type", e))?;
			want.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
				.map_err(|e| mf_err("set subtype", e))?;
			if let (Some(w), Some(h)) = (config.width, config.height) {
				want.SetUINT64(&MF_MT_FRAME_SIZE, pack_2x32(w, h))
					.map_err(|e| mf_err("set frame size", e))?;
			}
			if let Some(fps) = config.framerate {
				want.SetUINT64(&MF_MT_FRAME_RATE, pack_2x32(fps, 1))
					.map_err(|e| mf_err("set frame rate", e))?;
			}
			reader
				.SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &want)
				.map_err(|e| mf_err("set NV12 output type", e))?;
		}

		// Read back what we actually negotiated.
		let current = unsafe {
			reader
				.GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
				.map_err(|e| mf_err("get current media type", e))?
		};
		let frame_size = unsafe {
			current
				.GetUINT64(&MF_MT_FRAME_SIZE)
				.map_err(|e| mf_err("read frame size", e))?
		};
		let (width, height) = unpack_2x32(frame_size);
		// I420 chroma is 2x2 subsampled, so the encoder needs even dimensions.
		if width % 2 != 0 || height % 2 != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"camera resolution {width}x{height} must be even for H.264 encoding"
			)));
		}
		let framerate = unsafe { current.GetUINT64(&MF_MT_FRAME_RATE).ok() }.and_then(|packed| {
			let (num, den) = unpack_2x32(packed);
			(den != 0).then(|| (num / den).max(1))
		});

		tracing::info!(device = %device, width, height, framerate, "opened Media Foundation capture");
		Ok(Self {
			source,
			reader,
			width,
			height,
			framerate,
			device,
			_com: com,
		})
	}

	fn sample_to_i420(&self, sample: &IMFSample) -> Result<I420, Error> {
		let buffer = unsafe {
			sample
				.ConvertToContiguousBuffer()
				.map_err(|e| mf_err("contiguous buffer", e))?
		};

		// Prefer the 2D copy: it strips per-row stride padding, yielding canonical
		// tightly-packed NV12. Fall back to a flat lock if the buffer isn't 2D
		// (then we trust the buffer is already unpadded, i.e. stride == width).
		let nv12 = if let Ok(buf2d) = buffer.cast::<IMF2DBuffer>() {
			let len = unsafe {
				buf2d
					.GetContiguousLength()
					.map_err(|e| mf_err("contiguous length", e))?
			};
			let mut data = vec![0u8; len as usize];
			unsafe {
				buf2d
					.ContiguousCopyTo(&mut data)
					.map_err(|e| mf_err("contiguous copy", e))?;
			}
			data
		} else {
			let mut ptr_out: *mut u8 = ptr::null_mut();
			let mut current_len: u32 = 0;
			unsafe {
				buffer
					.Lock(&mut ptr_out, None, Some(&mut current_len))
					.map_err(|e| mf_err("lock buffer", e))?;
			}
			let data = unsafe { slice::from_raw_parts(ptr_out, current_len as usize) }.to_vec();
			unsafe {
				let _ = buffer.Unlock();
			}
			data
		};

		I420::from_nv12(&nv12, self.width, self.height)
	}
}

impl FrameSource for Camera {
	fn read(&mut self) -> Result<Option<Frame>, Error> {
		loop {
			let mut flags: u32 = 0;
			let mut sample: Option<IMFSample> = None;
			unsafe {
				self.reader
					.ReadSample(
						MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
						0,
						None,
						Some(&mut flags),
						None,
						Some(&mut sample),
					)
					.map_err(|e| mf_err("read sample", e))?;
			}

			if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
				return Ok(None);
			}
			// A null sample with no end-of-stream is a gap / stream tick (e.g. a
			// mid-stream format change); keep reading until a real frame arrives.
			let Some(sample) = sample else {
				continue;
			};
			return Ok(Some(Frame::I420(self.sample_to_i420(&sample)?)));
		}
	}

	fn width(&self) -> u32 {
		self.width
	}

	fn height(&self) -> u32 {
		self.height
	}

	fn framerate(&self) -> Option<u32> {
		self.framerate
	}

	fn device(&self) -> &str {
		&self.device
	}
}

impl Drop for Camera {
	fn drop(&mut self) {
		// Shut the media source so the camera releases promptly (LED off) when a
		// viewer leaves, rather than waiting on refcounted teardown.
		unsafe {
			let _ = self.source.Shutdown();
		}
	}
}

fn create_attributes(capacity: u32) -> Result<IMFAttributes, Error> {
	let mut attrs: Option<IMFAttributes> = None;
	unsafe {
		MFCreateAttributes(&mut attrs, capacity).map_err(|e| mf_err("create attributes", e))?;
	}
	attrs.ok_or_else(|| Error::Codec(anyhow::anyhow!("MFCreateAttributes returned null")))
}

/// Which device to open.
enum Selector {
	Index(usize),
	Name(String),
}

/// Enumerate video capture devices and activate the one matching `config.device`
/// (a bare integer selects by index, anything else is a friendly-name substring;
/// `None` opens index 0).
fn open_source(config: &Config) -> Result<(IMFMediaSource, String), Error> {
	let selector = match config.device.as_deref() {
		None => Selector::Index(0),
		Some(spec) => match spec.parse::<usize>() {
			Ok(i) => Selector::Index(i),
			Err(_) => Selector::Name(spec.to_string()),
		},
	};

	let attrs = create_attributes(1)?;
	unsafe {
		attrs
			.SetGUID(
				&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
				&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID,
			)
			.map_err(|e| mf_err("set device source type", e))?;
	}

	let mut activates: *mut Option<IMFActivate> = ptr::null_mut();
	let mut count: u32 = 0;
	unsafe {
		MFEnumDeviceSources(&attrs, &mut activates, &mut count).map_err(|e| mf_err("enumerate devices", e))?;
	}
	if count == 0 {
		return Err(Error::Codec(anyhow::anyhow!("no video capture devices found")));
	}

	// `MFEnumDeviceSources` hands back a CoTaskMemAlloc'd array, each entry holding
	// one ref we own. `take()` each into an owned handle so the unmatched ones drop
	// (release) here; the chosen one stays alive. Then free the array itself.
	let entries = unsafe { slice::from_raw_parts_mut(activates, count as usize) };
	let mut chosen: Option<(IMFActivate, String)> = None;
	for (i, slot) in entries.iter_mut().enumerate() {
		let Some(activate) = slot.take() else { continue };
		let name = unsafe { friendly_name(&activate) }.unwrap_or_else(|_| format!("camera {i}"));
		let matched = match &selector {
			Selector::Index(idx) => i == *idx,
			Selector::Name(want) => name.to_lowercase().contains(&want.to_lowercase()),
		};
		if matched && chosen.is_none() {
			chosen = Some((activate, name));
		}
	}
	unsafe {
		CoTaskMemFree(Some(activates as *const c_void));
	}

	let (activate, name) = chosen.ok_or_else(|| match &selector {
		Selector::Index(i) => Error::Codec(anyhow::anyhow!("camera index {i} out of range ({count} found)")),
		Selector::Name(n) => Error::Codec(anyhow::anyhow!("no camera matching {n:?} ({count} found)")),
	})?;

	let source: IMFMediaSource = unsafe { activate.ActivateObject().map_err(|e| mf_err("activate device", e))? };
	Ok((source, name))
}

/// Read a device's `MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME`, freeing the
/// COM-allocated string afterward.
unsafe fn friendly_name(activate: &IMFActivate) -> Result<String, Error> {
	let mut value = PWSTR::null();
	let mut len: u32 = 0;
	unsafe {
		activate
			.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut value, &mut len)
			.map_err(|e| mf_err("friendly name", e))?;
	}
	let name = unsafe { value.to_string() }.unwrap_or_default();
	unsafe {
		CoTaskMemFree(Some(value.0 as *const c_void));
	}
	Ok(name)
}
