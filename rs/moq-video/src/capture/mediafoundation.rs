//! Native Windows webcam capture via Media Foundation.
//!
//! Drives an [`IMFSourceReader`] over the selected capture device. When a
//! Direct3D11 device is available the reader runs with that device's DXGI
//! manager and the advanced video processor, so each sample arrives as a
//! GPU-resident NV12 texture ([`Frame::Texture`]) that the hardware encoder MFT
//! consumes zero-copy. Without a GPU (e.g. a headless VM) it falls back to the
//! source reader's software video processor, copying each sample to a packed CPU
//! [`I420`] ([`Frame::I420`]) the encoder uploads.

use std::ffi::c_void;
use std::ptr;
use std::slice;

use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
	IMF2DBuffer, IMFActivate, IMFAttributes, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaSource, IMFSample,
	IMFSourceReader, MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
	MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK,
	MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE, MF_SOURCE_READER_D3D_MANAGER,
	MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
	MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READERF_ENDOFSTREAM, MFCreateAttributes, MFCreateMediaType,
	MFCreateSourceReaderFromMediaSource, MFEnumDeviceSources, MFMediaType_Video, MFVideoFormat_NV12,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::core::{GUID, Interface, PWSTR};

use super::channel::FrameChannel;
use super::pump::{self, Geometry};
use super::{Config, FrameStream};
use crate::Error;
use crate::frame::d3d11::Texture;
use crate::frame::{Frame, I420};

/// List Media Foundation capture devices by their stable symbolic links.
pub(super) fn cameras() -> Result<Vec<super::Camera>, Error> {
	let _com = ComGuard::new()?;
	Ok(enumerate_sources()?
		.into_iter()
		.map(|source| super::Camera {
			id: source.id,
			name: source.name,
		})
		.collect())
}

/// Open a Media Foundation camera and stream its frames over a pump thread.
pub(super) async fn open(config: &Config, device: Option<&str>) -> Result<FrameStream, Error> {
	let config = config.clone();
	// The device opens on the pump thread, so the selector has to be owned.
	let device = device.map(str::to_string);
	let chan = FrameChannel::new();
	let (geo, guard) = pump::spawn(
		chan.clone(),
		move || {
			let camera = Camera::open(&config, device.as_deref())?;
			let geometry = Geometry {
				width: camera.width,
				height: camera.height,
				framerate: camera.framerate,
				device: camera.device_name.clone(),
			};
			Ok((camera, geometry))
		},
		Camera::read,
	)
	.await?;

	Ok(FrameStream::new(
		chan,
		geo.width,
		geo.height,
		geo.framerate,
		geo.device,
		None,
		Box::new(guard),
	))
}
use crate::mf::{ComGuard, create_d3d_device, mf_err, pack_2x32, unpack_2x32};

/// An open camera, read frame-by-frame via [`read`](Self::read) on the pump thread.
struct Camera {
	source: IMFMediaSource,
	reader: IMFSourceReader,
	/// The shared Direct3D11 device when capturing on the GPU; `None` on the CPU
	/// fallback. Its presence is what selects the texture vs I420 read path.
	device: Option<ID3D11Device>,
	width: u32,
	height: u32,
	framerate: Option<u32>,
	device_name: String,
	// Keep the DXGI manager alive for the reader's lifetime (the reader holds its
	// own ref, but we own the pairing). Drops before `_com`.
	_manager: Option<IMFDXGIDeviceManager>,
	// Drop last: tear down Media Foundation only after the reader/source release.
	_com: ComGuard,
}

impl Camera {
	fn open(config: &Config, selector: Option<&str>) -> Result<Self, Error> {
		let com = ComGuard::new()?;
		let (source, device_name) = open_source(selector)?;

		// Try for a GPU device; fall back to the CPU video processor if it (or any
		// of its setup) fails, e.g. on a headless VM with no D3D11 hardware.
		let gpu = match create_d3d_device() {
			Ok(gpu) => Some(gpu),
			Err(e) => {
				tracing::debug!(error = %e, "no D3D11 device; using CPU capture path");
				None
			}
		};

		let reader_attrs = create_attributes(2)?;
		unsafe {
			match &gpu {
				Some((_, manager)) => {
					// Bind the reader to our device and enable the advanced
					// (D3D-capable) video processor, so output stays a GPU texture.
					reader_attrs
						.SetUnknown(&MF_SOURCE_READER_D3D_MANAGER, manager)
						.map_err(|e| mf_err("set D3D manager", e))?;
					reader_attrs
						.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
						.map_err(|e| mf_err("enable advanced video processing", e))?;
				}
				None => {
					// Software video processor: converts the camera's native format
					// (MJPEG / YUY2 / ...) to NV12 in system memory.
					reader_attrs
						.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
						.map_err(|e| mf_err("enable video processing", e))?;
				}
			}
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

		let (device, manager) = match gpu {
			Some((device, manager)) => (Some(device), Some(manager)),
			None => (None, None),
		};
		tracing::info!(
			device = %device_name,
			width,
			height,
			framerate,
			gpu = device.is_some(),
			"opened Media Foundation capture"
		);
		Ok(Self {
			source,
			reader,
			device,
			width,
			height,
			framerate,
			device_name,
			_manager: manager,
			_com: com,
		})
	}

	/// Wrap a GPU sample's DXGI texture as a zero-copy [`Frame::Texture`].
	fn sample_to_texture(&self, device: &ID3D11Device, sample: &IMFSample) -> Result<Frame, Error> {
		let buffer = unsafe { sample.GetBufferByIndex(0).map_err(|e| mf_err("get buffer", e))? };
		let dxgi = buffer
			.cast::<IMFDXGIBuffer>()
			.map_err(|e| mf_err("buffer is not a DXGI surface", e))?;
		// GetResource returns a fresh ref (`AddRef`) we take ownership of.
		let mut raw: *mut c_void = ptr::null_mut();
		unsafe {
			dxgi.GetResource(&ID3D11Texture2D::IID, &mut raw)
				.map_err(|e| mf_err("get DXGI resource", e))?;
		}
		let texture = unsafe { ID3D11Texture2D::from_raw(raw) };
		let subresource = unsafe {
			dxgi.GetSubresourceIndex()
				.map_err(|e| mf_err("get subresource index", e))?
		};

		Ok(Frame::Texture(Texture::new(
			device.clone(),
			texture,
			subresource,
			self.width,
			self.height,
		)))
	}

	/// Copy a CPU sample's NV12 to a packed [`Frame::I420`] (the fallback path).
	fn sample_to_i420(&self, sample: &IMFSample) -> Result<Frame, Error> {
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

		Ok(Frame::I420(I420::from_nv12(&nv12, self.width, self.height)?))
	}

	/// Pull the next frame. Blocks per frame; the pump thread calls this in a loop
	/// and checks its stop flag between calls.
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
			let frame = match &self.device {
				Some(device) => self.sample_to_texture(device, &sample)?,
				None => self.sample_to_i420(&sample)?,
			};
			return Ok(Some(frame));
		}
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
	IdOrName(String),
}

/// Enumerate video capture devices and activate the one matching `selector`
/// (a bare integer selects by index, anything else is an exact symbolic link or
/// a friendly-name substring; `None` opens index 0).
fn open_source(selector: Option<&str>) -> Result<(IMFMediaSource, String), Error> {
	let selector = match selector {
		None => Selector::Index(0),
		Some(spec) => match spec.parse::<usize>() {
			Ok(i) => Selector::Index(i),
			Err(_) => Selector::IdOrName(spec.to_string()),
		},
	};

	let sources = enumerate_sources()?;
	let count = sources.len();
	let chosen = sources
		.into_iter()
		.enumerate()
		.find(|(index, source)| match &selector {
			Selector::Index(want) => *index == *want,
			Selector::IdOrName(want) => {
				source.id.eq_ignore_ascii_case(want) || source.name.to_lowercase().contains(&want.to_lowercase())
			}
		})
		.map(|(_, source)| source);

	let source = chosen.ok_or_else(|| match &selector {
		Selector::Index(i) => Error::Codec(anyhow::anyhow!("camera index {i} out of range ({count} found)")),
		Selector::IdOrName(value) => Error::Codec(anyhow::anyhow!("no camera matching {value:?} ({count} found)")),
	})?;

	let media: IMFMediaSource = unsafe {
		source
			.activate
			.ActivateObject()
			.map_err(|e| mf_err("activate device", e))?
	};
	Ok((media, source.name))
}

/// One Media Foundation capture source and the metadata exposed publicly.
struct SourceInfo {
	activate: IMFActivate,
	id: String,
	name: String,
}

/// Enumerate every Media Foundation video-capture source on the current thread.
fn enumerate_sources() -> Result<Vec<SourceInfo>, Error> {
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
		return Ok(Vec::new());
	}

	// `MFEnumDeviceSources` hands back a CoTaskMemAlloc'd array, each entry holding
	// one ref we own. `take()` each into an owned handle, then free the array.
	let entries = unsafe { slice::from_raw_parts_mut(activates, count as usize) };
	let mut sources = Vec::with_capacity(count as usize);
	for (i, slot) in entries.iter_mut().enumerate() {
		let Some(activate) = slot.take() else { continue };
		let name = unsafe { allocated_string(&activate, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME) }
			.unwrap_or_else(|_| format!("camera {i}"));
		let id = unsafe { allocated_string(&activate, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK) }
			.unwrap_or_else(|_| i.to_string());
		sources.push(SourceInfo { activate, id, name });
	}
	unsafe {
		CoTaskMemFree(Some(activates as *const c_void));
	}
	Ok(sources)
}

/// Read an allocated string attribute, freeing the COM allocation afterward.
unsafe fn allocated_string(activate: &IMFActivate, key: &GUID) -> Result<String, Error> {
	let mut value = PWSTR::null();
	let mut len: u32 = 0;
	unsafe {
		activate
			.GetAllocatedString(key, &mut value, &mut len)
			.map_err(|e| mf_err("capture source attribute", e))?;
	}
	let name = unsafe { value.to_string() }.unwrap_or_default();
	unsafe {
		CoTaskMemFree(Some(value.0 as *const c_void));
	}
	Ok(name)
}
