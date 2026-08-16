//! Hardware H.264 / H.265 decode backend via the Media Foundation decoder MFT +
//! DXVA.
//!
//! The inverse of the encode Media Foundation backend, and the Windows
//! counterpart to the macOS VideoToolbox decode backend. Unlike encoders, the
//! GPU vendors (NVIDIA especially) don't ship standalone async hardware decoder
//! MFTs; the portable hardware path is the Microsoft decoder MFT driven
//! synchronously with a Direct3D11 device manager bound to it, which routes the
//! actual decode to DXVA (NVDEC / Intel / AMD). We require that device: if it
//! can't be created (a GPU-less host), `open` fails. H.265 additionally needs an
//! HEVC decoder MFT present (the inbox HEVC Video Extensions or a vendor MFT);
//! absent that, `open` fails too. For H.264 the caller then falls back to
//! openh264; H.265 has no software fallback, so it simply has no decoder.
//!
//! Each Annex-B access unit goes in as an `IMFSample`; decoded NV12 comes back as
//! a GPU texture, which we blit into one of our own and hand out as
//! [`Surface::Texture`] so the picture stays on the GPU for a renderer or a
//! re-encode. It is downloaded to I420 only when a consumer asks. The blit is
//! what makes the frame ours rather than a slot in the decoder's pool; see
//! [`Texture::copy_from_sample`] for why a decoded picture can't just be handed
//! out.
//!
//! The MFT learns the picture size from the bitstream, so unlike the encoder we
//! don't know the dimensions up front: only after feeding the first access unit
//! does the MFT offer an output type carrying the real frame size, so we set the
//! NV12 output type (and read the size off it) right after that first input.
//!
//! Used only from the one decode task (the consumer's `read` loop), so the COM
//! handles are wrapped in a thread-confined `Send` type.

use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::ptr;

use bytes::Bytes;
use moq_net::Timestamp;
use windows::Win32::Graphics::Direct3D11::ID3D11Device;
use windows::Win32::Media::MediaFoundation::{
	IMF2DBuffer, IMFDXGIBuffer, IMFDXGIDeviceManager, IMFMediaType, IMFSample, IMFTransform, MF_E_NO_MORE_TYPES,
	MF_E_NOTACCEPTING, MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_LOW_LATENCY, MF_MT_FRAME_SIZE,
	MF_MT_MAJOR_TYPE, MF_MT_MINIMUM_DISPLAY_APERTURE, MF_MT_SUBTYPE, MFCreateMediaType, MFCreateMemoryBuffer,
	MFCreateSample, MFMediaType_Video, MFOffset, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_SORTANDFILTER,
	MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
	MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO,
	MFTEnumEx, MFVideoArea, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12,
};
use windows::core::{GUID, Interface};

use super::{Backend, Codec, Config};
use crate::frame::d3d11::Texture;
use crate::frame::{I420, Surface};
use crate::mf::{ComGuard, create_d3d_device, mf_err, unpack_2x32};
use crate::{Error, Frame};

pub(crate) const NAME: &str = "mediafoundation";

/// Media Foundation time base (100ns units per second). The decoder doesn't need
/// real timestamps (the container PTS is applied downstream), only monotonically
/// increasing ones to keep input samples in order, so we space them by a nominal
/// 30fps tick.
const HNS_PER_SEC: i64 = 10_000_000;
const NOMINAL_FPS: i64 = 30;

pub(crate) struct MediaFoundation {
	transform: IMFTransform,
	/// The Media Foundation input subtype (`MFVideoFormat_H264` / `_HEVC`), kept
	/// for `set_input_type`.
	input_subtype: GUID,
	/// The DXVA device backing the decoder; also the device the output textures
	/// live on, so the I420 download runs on the device that owns them.
	device: ID3D11Device,
	/// Picture size, learned from the first output-type negotiation (0 until then).
	width: u32,
	height: u32,
	/// True once the NV12 output type is set (after the initial stream change).
	output_configured: bool,
	/// Whether the MFT allocates its own output samples (true in DXVA mode: each
	/// output is a GPU texture from the decoder's pool). Cached from
	/// `GetOutputStreamInfo`.
	provides_samples: bool,
	/// Output buffer size to allocate when `provides_samples` is false.
	output_size: u32,
	sample_index: i64,
	/// Kept alive for the MFT's lifetime; the MFT holds its own ref but we own the
	/// pairing. Drops before `_com`.
	_manager: IMFDXGIDeviceManager,
	_com: ComGuard,
}

// The MFT and its COM handles are only ever touched from the one decode task (the
// consumer's single-threaded `read` loop).
unsafe impl Send for MediaFoundation {}

impl MediaFoundation {
	/// `config` is accepted for signature parity; the decoder MFT emits frames
	/// at the stream's native size (callers scale the frames themselves).
	pub(crate) fn open(codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		let (input_subtype, label) = match codec {
			Codec::H264 => (MFVideoFormat_H264, "H.264"),
			Codec::H265 => (MFVideoFormat_HEVC, "H.265"),
			Codec::Av1 => {
				return Err(Error::Codec(anyhow::anyhow!(
					"Media Foundation AV1 decode is not wired"
				)));
			}
		};
		let com = ComGuard::new()?;
		let transform = enumerate_decoder(input_subtype)?;

		// A hardware (DXVA) decode needs a D3D manager; failing here (no GPU) drops
		// us to the openh264 fallback, keeping this backend hardware-only.
		let (device, manager) = create_d3d_device()?;
		unsafe {
			transform
				.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)
				.map_err(|e| mf_err("set D3D manager", e))?;
		}

		// Disable the decoder's output-reorder buffer so each access unit produces
		// its frame right away. Our streams carry no B-frames, so this adds no
		// latency and avoids holding frames until an end-of-stream drain (which the
		// per-access-unit `Backend` interface never signals).
		let attrs = unsafe { transform.GetAttributes().map_err(|e| mf_err("MFT GetAttributes", e))? };
		unsafe {
			let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
		}

		let backend = Self {
			transform,
			input_subtype,
			device,
			width: 0,
			height: 0,
			output_configured: false,
			provides_samples: false,
			output_size: 0,
			sample_index: 0,
			_manager: manager,
			_com: com,
		};

		backend.set_input_type()?;
		unsafe {
			backend
				.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
				.map_err(|e| mf_err("begin streaming", e))?;
			// Async-model message; a synchronous decoder may not implement it, so
			// don't let that reject an otherwise-working MFT.
			let _ = backend.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
		}

		tracing::info!(decoder = NAME, codec = label, "opened decoder");
		Ok(Box::new(backend))
	}

	/// Describe the input as the coded format (H.264 / H.265); the MFT parses the
	/// bitstream for the rest (the picture size shows up later on the output type).
	fn set_input_type(&self) -> Result<(), Error> {
		let media = unsafe { MFCreateMediaType().map_err(|e| mf_err("create input type", e))? };
		unsafe {
			media
				.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
				.map_err(|e| mf_err("input major type", e))?;
			media
				.SetGUID(&MF_MT_SUBTYPE, &self.input_subtype)
				.map_err(|e| mf_err("input subtype", e))?;
			self.transform
				.SetInputType(0, &media, 0)
				.map_err(|e| mf_err("SetInputType", e))?;
		}
		Ok(())
	}

	/// Pick the NV12 output type the MFT offers (now that it has parsed the
	/// stream) and read the negotiated picture size off it. Called once after the
	/// first access unit, and again if the MFT later reports a resolution change.
	fn configure_output(&mut self) -> Result<(), Error> {
		let mut index = 0;
		loop {
			let media = match unsafe { self.transform.GetOutputAvailableType(0, index) } {
				Ok(media) => media,
				Err(e) if e.code() == MF_E_NO_MORE_TYPES => {
					return Err(Error::Codec(anyhow::anyhow!("decoder offers no NV12 output type")));
				}
				Err(e) => return Err(mf_err("GetOutputAvailableType", e)),
			};

			let subtype = unsafe { media.GetGUID(&MF_MT_SUBTYPE).map_err(|e| mf_err("output subtype", e))? };
			if subtype == MFVideoFormat_NV12 {
				unsafe {
					self.transform
						.SetOutputType(0, &media, 0)
						.map_err(|e| mf_err("SetOutputType", e))?;
				}
				self.read_frame_size(&media)?;
				self.read_output_info()?;
				self.output_configured = true;
				return Ok(());
			}
			index += 1;
		}
	}

	/// The size the pictures we hand out are: the display aperture where the stream
	/// declares one, and the coded size otherwise.
	///
	/// `MF_MT_FRAME_SIZE` is the coded size, which is whole macroblocks: a 1080p
	/// stream codes as 1088 rows and a 180-row one as 192. Those extra rows are
	/// padding the encoder invented, not picture, and the stream says so in its
	/// display aperture.
	fn read_frame_size(&mut self, media: &IMFMediaType) -> Result<(), Error> {
		let packed = unsafe {
			media
				.GetUINT64(&MF_MT_FRAME_SIZE)
				.map_err(|e| mf_err("frame size", e))?
		};
		let (coded_width, coded_height) = unpack_2x32(packed);
		// Clamped to what the decoder actually allocated, because the aperture comes
		// from the stream and the stream comes off the network. A larger one would
		// put the GPU copy's source region outside the decoder's texture, and
		// `CopySubresourceRegion` returns no status: the frame would report the
		// oversized dimensions over a texture nothing ever wrote.
		let (width, height) = match display_aperture(media) {
			Some((width, height)) => (width.min(coded_width), height.min(coded_height)),
			None => (coded_width, coded_height),
		};

		// I420 chroma is 2x2 subsampled, so the download needs even dimensions.
		if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"decoder reported unusable frame size {width}x{height}"
			)));
		}
		self.width = width;
		self.height = height;
		Ok(())
	}

	/// Cache whether the MFT provides its own output samples (true for a
	/// DXVA-backed decoder: each output is a GPU texture it allocates) and, if
	/// not, how big a buffer we must hand it.
	fn read_output_info(&mut self) -> Result<(), Error> {
		let info = unsafe {
			self.transform
				.GetOutputStreamInfo(0)
				.map_err(|e| mf_err("GetOutputStreamInfo", e))?
		};
		self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;
		self.output_size = info.cbSize;
		Ok(())
	}

	/// Wrap an Annex-B access unit as an input [`IMFSample`] in a fresh
	/// system-memory buffer (the MFT uploads it to the GPU itself).
	fn build_sample(&self, access_unit: &[u8]) -> Result<IMFSample, Error> {
		let buffer = unsafe { MFCreateMemoryBuffer(access_unit.len() as u32).map_err(|e| mf_err("input buffer", e))? };
		let mut ptr_out: *mut u8 = ptr::null_mut();
		unsafe {
			buffer
				.Lock(&mut ptr_out, None, None)
				.map_err(|e| mf_err("lock input buffer", e))?;
		}
		// SAFETY: the buffer was allocated with `access_unit.len()` bytes and we
		// hold the lock until `Unlock`.
		unsafe { std::slice::from_raw_parts_mut(ptr_out, access_unit.len()) }.copy_from_slice(access_unit);
		unsafe {
			let _ = buffer.Unlock();
			buffer
				.SetCurrentLength(access_unit.len() as u32)
				.map_err(|e| mf_err("set input length", e))?;
		}

		let sample = unsafe { MFCreateSample().map_err(|e| mf_err("MFCreateSample", e))? };
		unsafe {
			sample.AddBuffer(&buffer).map_err(|e| mf_err("AddBuffer", e))?;
			let tick = HNS_PER_SEC / NOMINAL_FPS;
			sample
				.SetSampleTime(self.sample_index * tick)
				.map_err(|e| mf_err("SetSampleTime", e))?;
			sample
				.SetSampleDuration(tick)
				.map_err(|e| mf_err("SetSampleDuration", e))?;
		}
		Ok(sample)
	}

	/// Pull every frame the MFT has ready, stopping when it asks for more input.
	/// No-op until the output type is configured (the decoder rejects
	/// `ProcessOutput` before then).
	///
	/// Reports whether the decoder got anywhere, which is not the same as whether
	/// it handed back a picture: renegotiating the output type is progress too, and
	/// it produces no frame.
	fn drain_output(&mut self, out: &mut Vec<Surface>) -> Result<bool, Error> {
		if !self.output_configured {
			return Ok(false);
		}
		let mut progressed = false;
		while self.process_output(out)? {
			progressed = true;
		}
		Ok(progressed)
	}

	/// One `ProcessOutput`. Returns `true` if it produced a frame or handled a
	/// stream change (so the caller keeps draining), `false` on
	/// `MF_E_TRANSFORM_NEED_MORE_INPUT` (the decoder wants the next access unit).
	fn process_output(&mut self, out: &mut Vec<Surface>) -> Result<bool, Error> {
		// In DXVA mode the MFT provides its own texture-backed sample, so we pass a
		// null slot; otherwise we hand it a system-memory buffer to fill.
		let provided = if self.provides_samples {
			None
		} else {
			let buffer = unsafe { MFCreateMemoryBuffer(self.output_size).map_err(|e| mf_err("output buffer", e))? };
			let sample = unsafe { MFCreateSample().map_err(|e| mf_err("output sample", e))? };
			unsafe { sample.AddBuffer(&buffer).map_err(|e| mf_err("output AddBuffer", e))? };
			Some(sample)
		};

		let mut data = [MFT_OUTPUT_DATA_BUFFER {
			dwStreamID: 0,
			pSample: ManuallyDrop::new(provided),
			dwStatus: 0,
			pEvents: ManuallyDrop::new(None),
		}];
		let mut status = 0u32;
		let result = unsafe { self.transform.ProcessOutput(0, &mut data, &mut status) };

		let sample = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pSample) });
		let _events = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pEvents) });

		match result {
			Ok(()) => {
				if let Some(sample) = sample {
					out.push(self.sample_to_surface(&sample)?);
				}
				Ok(true)
			}
			Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(false),
			Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
				self.configure_output()?;
				Ok(true)
			}
			Err(e) => Err(mf_err("ProcessOutput", e)),
		}
	}

	/// Wrap a decoded output sample as a [`Surface`]. The DXVA path keeps the
	/// picture on the GPU, blitting it out of the decoder's pool into a texture of
	/// ours; a system-memory fallback deinterleaves the contiguous NV12 to packed
	/// I420.
	fn sample_to_surface(&self, sample: &IMFSample) -> Result<Surface, Error> {
		let buffer = unsafe { sample.GetBufferByIndex(0).map_err(|e| mf_err("get output buffer", e))? };

		if buffer.cast::<IMFDXGIBuffer>().is_ok() {
			let texture = Texture::copy_from_sample(&self.device, sample, self.width, self.height)?;
			return Ok(Surface::Texture(texture));
		}

		// System-memory NV12: prefer the 2D copy (strips per-row stride padding).
		let buf2d = buffer
			.cast::<IMF2DBuffer>()
			.map_err(|e| mf_err("output buffer is neither DXGI nor 2D", e))?;
		let len = unsafe {
			buf2d
				.GetContiguousLength()
				.map_err(|e| mf_err("contiguous length", e))?
		};
		let mut nv12 = vec![0u8; len as usize];
		unsafe {
			buf2d
				.ContiguousCopyTo(&mut nv12)
				.map_err(|e| mf_err("contiguous copy", e))?;
		}
		Ok(Surface::I420(I420::from_nv12(&nv12, self.width, self.height)?))
	}
}

impl Backend for MediaFoundation {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
		let mut out = Vec::new();

		let sample = self.build_sample(&access_unit)?;
		// The decoder accepts one access unit at a time. If it's still holding
		// output it returns NOTACCEPTING; drain that, then the input goes in.
		loop {
			match unsafe { self.transform.ProcessInput(0, &sample, 0) } {
				Ok(()) => break,
				Err(e) if e.code() == MF_E_NOTACCEPTING => {
					// A drain that gets nowhere means the MFT will not take this access
					// unit and has nothing to hand back either, so retrying can only
					// spin. Report it instead of hanging the decode task.
					//
					// Progress, not frames: a stream that changes resolution mid-track
					// renegotiates the output type here and produces no picture doing
					// it, and that is exactly the case where feeding the same access
					// unit again is the right move.
					if !self.drain_output(&mut out)? {
						return Err(Error::Codec(anyhow::anyhow!(
							"decoder rejected the access unit with no output to drain"
						)));
					}
				}
				Err(e) => return Err(mf_err("ProcessInput", e)),
			}
		}
		self.sample_index += 1;

		// The decoder only reports the picture size once it has parsed the first
		// access unit's SPS, so the output type is configured here rather than at
		// open time.
		if !self.output_configured {
			self.configure_output()?;
		}

		self.drain_output(&mut out)?;
		// The synchronous MFT drains within the call, so every output frame
		// belongs to the access unit just submitted.
		Ok(out.into_iter().map(|surface| Frame::new(surface, timestamp)).collect())
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// The picture region of a decoder output type, from its minimum display
/// aperture: the part of the coded frame that is actually the image.
///
/// `None` when the stream declares none, and when it puts the picture at an
/// offset rather than the top left. Nothing produces the latter in practice (the
/// cropping that turns 1088 coded rows into 1080 trims the bottom), and taking
/// the whole coded frame keeps the picture inside it rather than sliding it.
fn display_aperture(media: &IMFMediaType) -> Option<(u32, u32)> {
	let mut area = MFVideoArea::default();
	// SAFETY: the attribute is a blob of exactly this struct, and the slice
	// covers the value we are filling in.
	let bytes =
		unsafe { std::slice::from_raw_parts_mut(std::ptr::from_mut(&mut area).cast::<u8>(), size_of::<MFVideoArea>()) };
	unsafe { media.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, bytes, None) }.ok()?;

	// A 16.16 fixed-point offset; anything but zero means the picture starts
	// somewhere we do not crop to.
	let origin = |offset: MFOffset| offset.value == 0 && offset.fract == 0;
	if !origin(area.OffsetX) || !origin(area.OffsetY) {
		return None;
	}
	let width = u32::try_from(area.Area.cx).ok()?;
	let height = u32::try_from(area.Area.cy).ok()?;
	(width > 0 && height > 0).then_some((width, height))
}

/// Pick the first synchronous decoder MFT (`subtype` in, NV12 out). The Microsoft
/// decoder it returns runs the actual decode on the GPU via DXVA once a D3D
/// manager is bound.
fn enumerate_decoder(subtype: GUID) -> Result<IMFTransform, Error> {
	let input = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: subtype,
	};
	let output = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: MFVideoFormat_NV12,
	};

	let mut activates: *mut Option<windows::Win32::Media::MediaFoundation::IMFActivate> = ptr::null_mut();
	let mut count: u32 = 0;
	unsafe {
		MFTEnumEx(
			MFT_CATEGORY_VIDEO_DECODER,
			MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
			Some(&input),
			Some(&output),
			&mut activates,
			&mut count,
		)
		.map_err(|e| mf_err("MFTEnumEx", e))?;
	}
	if count == 0 {
		return Err(Error::Codec(anyhow::anyhow!("no decoder MFT found")));
	}

	let entries = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
	let mut transform: Option<IMFTransform> = None;
	for slot in entries.iter_mut() {
		let Some(activate) = slot.take() else { continue };
		if transform.is_none()
			&& let Ok(mft) = unsafe { activate.ActivateObject::<IMFTransform>() }
		{
			transform = Some(mft);
		}
	}
	unsafe {
		windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const c_void));
	}

	transform.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to activate decoder MFT")))
}
