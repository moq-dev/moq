//! Hardware H.264 backend via a Media Foundation encoder MFT.
//!
//! Enumerates a hardware (`MFT_ENUM_FLAG_HARDWARE`) H.264 encoder and drives it
//! through the async-MFT event model. When capture hands us a [`Frame::Texture`]
//! the encoder runs on that texture's Direct3D11 device (via a DXGI device
//! manager) and consumes the surface zero-copy; a CPU [`Frame::I420`] is uploaded
//! into a system-memory NV12 sample instead.
//!
//! The MFT emits an Annex-B byte stream for `MFVideoFormat_H264`, with SPS/PPS
//! inline ahead of each IDR, which is exactly what `moq_mux` avc3 mode wants, so
//! unlike VideoToolbox there's no AVCC -> Annex-B rewrite. Used only from the one
//! capture/encode thread, so the COM handles are wrapped in a thread-confined
//! `Send` type.

use std::mem::ManuallyDrop;
use std::ptr;

use bytes::Bytes;
use windows::Win32::Foundation::{VARIANT_BOOL, VARIANT_TRUE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
	CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVGOPSize,
	CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFDXGIDeviceManager, IMFMediaBuffer,
	IMFMediaEventGenerator, IMFSample, IMFTransform, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
	MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT, MF_EVENT_FLAG_NONE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
	MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE,
	MF_TRANSFORM_ASYNC_UNLOCK, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
	MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE,
	MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
	MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER,
	MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264,
	MFVideoFormat_NV12, MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::Interface;

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::Frame;

pub(crate) const NAME: &str = "mediafoundation";

/// Stream tick for sample timestamps, in 100ns units (the Media Foundation time
/// base). Timestamps only need to increase; the moq timestamp is applied
/// downstream, so a monotonic index over the framerate is enough.
const HNS_PER_SEC: i64 = 10_000_000;

fn mf_err(ctx: &str, e: windows::core::Error) -> Error {
	Error::Codec(anyhow::anyhow!("{ctx}: {e}"))
}

/// COM (MTA) + Media Foundation lifetime for the encode thread, balanced on
/// drop. Refcounted, so it nests fine with the capture source's own guard when
/// both run on the same blocking thread.
struct ComGuard;

impl ComGuard {
	fn new() -> Result<Self, Error> {
		unsafe {
			CoInitializeEx(None, COINIT_MULTITHREADED)
				.ok()
				.map_err(|e| mf_err("CoInitializeEx", e))?;
			windows::Win32::Media::MediaFoundation::MFStartup(
				windows::Win32::Media::MediaFoundation::MF_VERSION,
				windows::Win32::Media::MediaFoundation::MFSTARTUP_FULL,
			)
			.map_err(|e| mf_err("MFStartup", e))?;
		}
		Ok(Self)
	}
}

impl Drop for ComGuard {
	fn drop(&mut self) {
		unsafe {
			let _ = windows::Win32::Media::MediaFoundation::MFShutdown();
			CoUninitialize();
		}
	}
}

pub(crate) struct MediaFoundation {
	transform: IMFTransform,
	events: IMFMediaEventGenerator,
	codec_api: ICodecAPI,
	width: u32,
	height: u32,
	framerate: u32,
	bitrate: u32,
	gop: u32,
	/// Lazily configured on the first frame, since the Direct3D11 device to bind
	/// (for zero-copy texture input) comes from the frame itself.
	started: bool,
	/// The MFT allocates its own output samples (true for hardware encoders).
	provides_samples: bool,
	/// True once the MFT has asked for input and we haven't fed it since.
	needs_input: bool,
	sample_index: i64,
	/// Kept alive for the MFT's lifetime once a texture frame binds a device.
	_manager: Option<IMFDXGIDeviceManager>,
	_com: ComGuard,
}

// The MFT and its COM handles are only ever touched from the one capture/encode
// thread (see `publish_capture`'s `spawn_blocking`).
unsafe impl Send for MediaFoundation {}

impl MediaFoundation {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let com = ComGuard::new()?;
		let transform = enumerate_encoder()?;

		// Unlock the async interface before any other use (hardware MFTs are async).
		let attrs = unsafe { transform.GetAttributes().map_err(|e| mf_err("MFT GetAttributes", e))? };
		unsafe {
			attrs
				.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
				.map_err(|e| mf_err("async unlock", e))?;
		}

		let events = transform
			.cast::<IMFMediaEventGenerator>()
			.map_err(|e| mf_err("MFT is not an event generator", e))?;
		let codec_api = transform
			.cast::<ICodecAPI>()
			.map_err(|e| mf_err("MFT has no ICodecAPI", e))?;

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Box::new(Self {
			transform,
			events,
			codec_api,
			width: config.width,
			height: config.height,
			framerate: config.framerate,
			bitrate: clamp_u32(config.resolved_bitrate()),
			gop: config.gop,
			started: false,
			provides_samples: false,
			needs_input: false,
			sample_index: 0,
			_manager: None,
			_com: com,
		}))
	}

	/// One-time configuration, deferred to the first frame so a texture frame can
	/// bind its own Direct3D11 device for zero-copy input.
	fn start(&mut self, frame: &Frame) -> Result<(), Error> {
		// Bind the frame's D3D11 device when it's a texture, so the MFT reads the
		// captured surface directly. A CPU frame runs the MFT in system memory.
		if let Frame::Texture(texture) = frame {
			let manager = device_manager(&texture.device)?;
			let raw = manager.as_raw() as usize;
			unsafe {
				self.transform
					.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, raw)
					.map_err(|e| mf_err("set D3D manager", e))?;
			}
			self._manager = Some(manager);
		}

		self.configure_codec_api()?;
		self.set_output_type()?;
		self.set_input_type()?;

		let info = unsafe {
			self.transform
				.GetOutputStreamInfo(0)
				.map_err(|e| mf_err("GetOutputStreamInfo", e))?
		};
		self.provides_samples = info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0;

		unsafe {
			self.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
				.map_err(|e| mf_err("begin streaming", e))?;
			self.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
				.map_err(|e| mf_err("start of stream", e))?;
		}
		self.started = true;
		Ok(())
	}

	fn configure_codec_api(&self) -> Result<(), Error> {
		// Low latency: no B-frames / lookahead, so output tracks input closely.
		self.set_codec(&CODECAPI_AVLowLatencyMode, variant_bool(true))?;
		self.set_codec(
			&CODECAPI_AVEncCommonRateControlMode,
			variant_u32(eAVEncCommonRateControlMode_CBR.0 as u32),
		)?;
		self.set_codec(&CODECAPI_AVEncCommonMeanBitRate, variant_u32(self.bitrate))?;
		self.set_codec(&CODECAPI_AVEncMPVGOPSize, variant_u32(self.gop))?;
		Ok(())
	}

	fn set_codec(&self, api: *const windows::core::GUID, value: VARIANT) -> Result<(), Error> {
		// Some knobs are advisory; a failure here shouldn't sink the encoder, but
		// it's worth surfacing in logs.
		if let Err(e) = unsafe { self.codec_api.SetValue(api, &value) } {
			tracing::debug!(error = %e, "encoder codec-api set failed");
		}
		Ok(())
	}

	fn set_output_type(&self) -> Result<(), Error> {
		let media = unsafe { MFCreateMediaType().map_err(|e| mf_err("create output type", e))? };
		unsafe {
			media
				.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
				.map_err(|e| mf_err("output major type", e))?;
			media
				.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
				.map_err(|e| mf_err("output subtype", e))?;
			media
				.SetUINT32(&MF_MT_AVG_BITRATE, self.bitrate)
				.map_err(|e| mf_err("output bitrate", e))?;
			media
				.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
				.map_err(|e| mf_err("output interlace", e))?;
			media
				.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
				.map_err(|e| mf_err("output profile", e))?;
			media
				.SetUINT64(&MF_MT_FRAME_SIZE, pack_2x32(self.width, self.height))
				.map_err(|e| mf_err("output frame size", e))?;
			media
				.SetUINT64(&MF_MT_FRAME_RATE, pack_2x32(self.framerate, 1))
				.map_err(|e| mf_err("output frame rate", e))?;
			self.transform
				.SetOutputType(0, &media, 0)
				.map_err(|e| mf_err("SetOutputType", e))?;
		}
		Ok(())
	}

	fn set_input_type(&self) -> Result<(), Error> {
		let media = unsafe { MFCreateMediaType().map_err(|e| mf_err("create input type", e))? };
		unsafe {
			media
				.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
				.map_err(|e| mf_err("input major type", e))?;
			media
				.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
				.map_err(|e| mf_err("input subtype", e))?;
			media
				.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
				.map_err(|e| mf_err("input interlace", e))?;
			media
				.SetUINT64(&MF_MT_FRAME_SIZE, pack_2x32(self.width, self.height))
				.map_err(|e| mf_err("input frame size", e))?;
			media
				.SetUINT64(&MF_MT_FRAME_RATE, pack_2x32(self.framerate, 1))
				.map_err(|e| mf_err("input frame rate", e))?;
			self.transform
				.SetInputType(0, &media, 0)
				.map_err(|e| mf_err("SetInputType", e))?;
		}
		Ok(())
	}

	/// Wrap a captured frame as an input [`IMFSample`]: a zero-copy DXGI surface
	/// buffer for a texture, or a freshly uploaded NV12 memory buffer for I420.
	fn build_sample(&self, frame: &Frame) -> Result<IMFSample, Error> {
		let buffer = match frame {
			Frame::Texture(texture) => unsafe {
				let buffer =
					MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture.texture, texture.subresource, false)
						.map_err(|e| mf_err("MFCreateDXGISurfaceBuffer", e))?;
				let length = buffer
					.cast::<windows::Win32::Media::MediaFoundation::IMF2DBuffer>()
					.map_err(|e| mf_err("DXGI buffer is not 2D", e))?
					.GetContiguousLength()
					.map_err(|e| mf_err("DXGI contiguous length", e))?;
				buffer
					.SetCurrentLength(length)
					.map_err(|e| mf_err("set DXGI length", e))?;
				buffer
			},
			Frame::I420(_) => self.upload_nv12(frame)?,
			#[allow(unreachable_patterns)]
			_ => {
				return Err(Error::Codec(anyhow::anyhow!(
					"unsupported frame for mediafoundation encoder"
				)));
			}
		};

		let sample = unsafe { MFCreateSample().map_err(|e| mf_err("MFCreateSample", e))? };
		unsafe {
			sample.AddBuffer(&buffer).map_err(|e| mf_err("AddBuffer", e))?;
			let tick = HNS_PER_SEC / self.framerate.max(1) as i64;
			sample
				.SetSampleTime(self.sample_index * tick)
				.map_err(|e| mf_err("SetSampleTime", e))?;
			sample
				.SetSampleDuration(tick)
				.map_err(|e| mf_err("SetSampleDuration", e))?;
		}
		Ok(sample)
	}

	/// Copy a CPU I420 frame into a system-memory NV12 buffer (the fallback when
	/// capture isn't producing GPU textures).
	fn upload_nv12(&self, frame: &Frame) -> Result<IMFMediaBuffer, Error> {
		let i420 = frame.to_i420()?;
		let (w, h) = (self.width as usize, self.height as usize);
		let (cw, ch) = (w / 2, h / 2);
		let len = w * h + 2 * cw * ch;

		let buffer = unsafe { MFCreateMemoryBuffer(len as u32).map_err(|e| mf_err("MFCreateMemoryBuffer", e))? };
		let mut ptr_out: *mut u8 = ptr::null_mut();
		unsafe {
			buffer
				.Lock(&mut ptr_out, None, None)
				.map_err(|e| mf_err("lock NV12 buffer", e))?;
		}
		// Y plane verbatim, then interleave U/V into the NV12 chroma plane.
		let (u, v) = (i420.u(), i420.v());
		unsafe {
			ptr::copy_nonoverlapping(i420.y().as_ptr(), ptr_out, w * h);
			let uv = ptr_out.add(w * h);
			for i in 0..cw * ch {
				*uv.add(i * 2) = u[i];
				*uv.add(i * 2 + 1) = v[i];
			}
			let _ = buffer.Unlock();
			buffer
				.SetCurrentLength(len as u32)
				.map_err(|e| mf_err("set NV12 length", e))?;
		}
		Ok(buffer)
	}

	/// Block on events until the MFT is ready for input, collecting any output
	/// that arrives meanwhile.
	fn wait_for_input(&mut self, out: &mut Vec<Bytes>) -> Result<(), Error> {
		while !self.needs_input {
			let event = unsafe {
				self.events
					.GetEvent(MF_EVENT_FLAG_NONE)
					.map_err(|e| mf_err("GetEvent", e))?
			};
			self.handle_event(&event, out)?;
		}
		Ok(())
	}

	/// Drain events already queued without blocking (called after feeding input).
	fn drain_ready(&mut self, out: &mut Vec<Bytes>) -> Result<(), Error> {
		loop {
			match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
				Ok(event) => self.handle_event(&event, out)?,
				Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
				Err(e) => return Err(mf_err("GetEvent (drain)", e)),
			}
		}
	}

	fn handle_event(
		&mut self,
		event: &windows::Win32::Media::MediaFoundation::IMFMediaEvent,
		out: &mut Vec<Bytes>,
	) -> Result<(), Error> {
		let kind = unsafe { event.GetType().map_err(|e| mf_err("event GetType", e))? };
		// METransformNeedInput / METransformHaveOutput aren't exposed as named
		// constants in this binding; their numeric values are part of the ABI.
		const NEED_INPUT: u32 = 601;
		const HAVE_OUTPUT: u32 = 602;
		match kind {
			NEED_INPUT => self.needs_input = true,
			HAVE_OUTPUT => {
				if let Some(packet) = self.process_output()? {
					out.push(packet);
				}
			}
			_ => {}
		}
		Ok(())
	}

	/// Pull one encoded access unit. Returns `None` if the MFT had nothing ready
	/// or asked us to renegotiate the output type.
	fn process_output(&mut self) -> Result<Option<Bytes>, Error> {
		let provided = if self.provides_samples {
			None
		} else {
			let info = unsafe {
				self.transform
					.GetOutputStreamInfo(0)
					.map_err(|e| mf_err("GetOutputStreamInfo", e))?
			};
			let buffer = unsafe { MFCreateMemoryBuffer(info.cbSize).map_err(|e| mf_err("output buffer", e))? };
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

		// Take ownership of whatever sample slot now holds (ours or the MFT's),
		// and release any event collection the MFT attached.
		let sample = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pSample) });
		let _events = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pEvents) });

		match result {
			Ok(()) => {}
			Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
			Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
				// The encoder revised its output format; re-apply ours and retry
				// on the next event.
				self.set_output_type()?;
				return Ok(None);
			}
			Err(e) => return Err(mf_err("ProcessOutput", e)),
		}

		let Some(sample) = sample else { return Ok(None) };
		Ok(Some(sample_to_bytes(&sample)?))
	}
}

impl Backend for MediaFoundation {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		if !self.started {
			self.start(frame)?;
		}

		let mut out = Vec::new();
		self.wait_for_input(&mut out)?;

		if keyframe {
			self.set_codec(&CODECAPI_AVEncVideoForceKeyFrame, variant_u32(1))?;
		}

		let sample = self.build_sample(frame)?;
		unsafe {
			self.transform
				.ProcessInput(0, &sample, 0)
				.map_err(|e| mf_err("ProcessInput", e))?;
		}
		self.needs_input = false;
		self.sample_index += 1;

		self.drain_ready(&mut out)?;
		Ok(out)
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		if !self.started {
			return Ok(Vec::new());
		}
		unsafe {
			let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
			let _ = self.transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0);
		}
		// Low-latency CBR buffers almost nothing, so a non-blocking sweep of the
		// queued events flushes the tail without risking a hang.
		let mut out = Vec::new();
		self.drain_ready(&mut out)?;
		Ok(out)
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// Pick the first hardware H.264 encoder MFT (NV12 in, H.264 out).
fn enumerate_encoder() -> Result<IMFTransform, Error> {
	let input = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: MFVideoFormat_NV12,
	};
	let output = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: MFVideoFormat_H264,
	};

	let mut activates: *mut Option<windows::Win32::Media::MediaFoundation::IMFActivate> = ptr::null_mut();
	let mut count: u32 = 0;
	unsafe {
		MFTEnumEx(
			MFT_CATEGORY_VIDEO_ENCODER,
			MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
			Some(&input),
			Some(&output),
			&mut activates,
			&mut count,
		)
		.map_err(|e| mf_err("MFTEnumEx", e))?;
	}
	if count == 0 {
		return Err(Error::Codec(anyhow::anyhow!("no hardware H.264 encoder found")));
	}

	let entries = unsafe { std::slice::from_raw_parts_mut(activates, count as usize) };
	let mut transform: Option<IMFTransform> = None;
	for slot in entries.iter_mut() {
		let Some(activate) = slot.take() else { continue };
		if transform.is_none() {
			if let Ok(mft) = unsafe { activate.ActivateObject::<IMFTransform>() } {
				transform = Some(mft);
			}
		}
	}
	unsafe {
		windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const std::ffi::c_void));
	}

	transform.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to activate H.264 encoder MFT")))
}

/// A DXGI device manager wrapping `device`, so the MFT shares the capture
/// device and reads its textures directly.
fn device_manager(device: &ID3D11Device) -> Result<IMFDXGIDeviceManager, Error> {
	let mut token: u32 = 0;
	let mut manager: Option<IMFDXGIDeviceManager> = None;
	unsafe {
		MFCreateDXGIDeviceManager(&mut token, &mut manager).map_err(|e| mf_err("MFCreateDXGIDeviceManager", e))?;
	}
	let manager = manager.ok_or_else(|| Error::Codec(anyhow::anyhow!("MFCreateDXGIDeviceManager returned null")))?;
	unsafe {
		manager
			.ResetDevice(device, token)
			.map_err(|e| mf_err("ResetDevice", e))?;
	}
	Ok(manager)
}

/// Copy an output sample's contiguous Annex-B bytes into an owned [`Bytes`].
fn sample_to_bytes(sample: &IMFSample) -> Result<Bytes, Error> {
	let buffer = unsafe {
		sample
			.ConvertToContiguousBuffer()
			.map_err(|e| mf_err("output contiguous buffer", e))?
	};
	let mut ptr_out: *mut u8 = ptr::null_mut();
	let mut len: u32 = 0;
	unsafe {
		buffer
			.Lock(&mut ptr_out, None, Some(&mut len))
			.map_err(|e| mf_err("lock output", e))?;
	}
	let bytes = Bytes::copy_from_slice(unsafe { std::slice::from_raw_parts(ptr_out, len as usize) });
	unsafe {
		let _ = buffer.Unlock();
	}
	Ok(bytes)
}

fn pack_2x32(hi: u32, lo: u32) -> u64 {
	((hi as u64) << 32) | lo as u64
}

fn clamp_u32(value: u64) -> u32 {
	value.min(u32::MAX as u64) as u32
}

fn variant_u32(value: u32) -> VARIANT {
	let mut variant = VARIANT::default();
	// SAFETY: write the union field that matches the tag we set.
	unsafe {
		let inner = &mut variant.Anonymous.Anonymous;
		inner.vt = VT_UI4;
		inner.Anonymous.ulVal = value;
	}
	variant
}

fn variant_bool(value: bool) -> VARIANT {
	let mut variant = VARIANT::default();
	unsafe {
		let inner = &mut variant.Anonymous.Anonymous;
		inner.vt = VT_BOOL;
		inner.Anonymous.boolVal = if value { VARIANT_TRUE } else { VARIANT_BOOL(0) };
	}
	variant
}
