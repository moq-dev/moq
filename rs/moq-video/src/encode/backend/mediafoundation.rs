//! Hardware H.264 / H.265 backend via a Media Foundation encoder MFT.
//!
//! Enumerates a hardware (`MFT_ENUM_FLAG_HARDWARE`) encoder for the requested
//! codec and drives it through the async-MFT event model. When capture hands us
//! a [`Surface::Texture`] the encoder runs on that texture's Direct3D11 device (via
//! a DXGI device manager) and consumes the surface zero-copy; a CPU
//! [`Surface::I420`] is uploaded into a system-memory NV12 sample instead.
//!
//! The MFT emits an Annex-B byte stream with parameter sets inline ahead of each
//! IDR/IRAP (SPS/PPS for H.264, VPS/SPS/PPS for H.265), which is exactly what
//! `moq_mux` avc3 / hev1 mode wants, so unlike VideoToolbox there's no
//! AVCC/HVCC -> Annex-B rewrite. The whole encoder lives on the dedicated encode
//! thread (see `encode::sink`), so its COM apartment stays balanced and its
//! blocking waits never park a tokio worker.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::ptr;

use bytes::Bytes;
use moq_net::Timestamp;
use windows::Win32::Foundation::{VARIANT_BOOL, VARIANT_TRUE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
	CODECAPI_AVEncCommonMeanBitRate, CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVGOPSize,
	CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFDXGIDeviceManager, IMFMediaBuffer,
	IMFMediaEvent, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform, METransformDrainComplete,
	METransformHaveOutput, METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
	MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT, MF_EVENT_FLAG_NONE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
	MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE,
	MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX,
	MF_TRANSFORM_ASYNC_UNLOCK, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
	MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFNominalRange_0_255, MFNominalRange_16_235,
	MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN,
	MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
	MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO,
	MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoInterlace_Progressive,
	MFVideoPrimaries_BT709, MFVideoPrimaries_SMPTE170M, MFVideoTransFunc_709, MFVideoTransferMatrix_BT601,
	MFVideoTransferMatrix_BT709, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High,
	eAVEncH265VProfile_Main_420_8,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_UI4};
use windows::core::{GUID, Interface};

use super::super::encoder::{Codec, Config};
use super::{Backend, Encoded};
use crate::frame::{Surface, interleave_uv};
use crate::mf::{ComGuard, mf_err, pack_2x32};
use crate::{Color, Error, Frame};

pub(crate) const NAME: &str = "mediafoundation";

/// Stream tick for sample timestamps, in 100ns units (the Media Foundation time
/// base). The codec only needs a clock that increases, so a monotonic index over
/// the framerate is enough; the moq timestamp rides alongside in `pending` and the
/// MFT echoes the sample time on its output, which is what pairs the two back up.
const HNS_PER_SEC: i64 = 10_000_000;

pub(crate) struct MediaFoundation {
	transform: IMFTransform,
	events: IMFMediaEventGenerator,
	codec_api: ICodecAPI,
	codec: Codec,
	width: u32,
	height: u32,
	framerate: u32,
	bitrate: u32,
	gop: u32,
	/// The color space of the input frames, stamped onto both media types so the
	/// encoder writes it into the bitstream's VUI.
	color: Color,
	/// Lazily configured on the first frame, since the Direct3D11 device to bind
	/// (for zero-copy texture input) comes from the frame itself.
	started: bool,
	/// The MFT allocates its own output samples (true for hardware encoders).
	provides_samples: bool,
	/// Output buffer size we must allocate when `provides_samples` is false.
	/// Cached from `GetOutputStreamInfo` so the output hot path doesn't re-query.
	output_size: u32,
	/// True once the MFT has asked for input and we haven't fed it since.
	needs_input: bool,
	/// True once the MFT has reported its drain complete, so a drain knows the
	/// tail is out rather than still coming.
	drained: bool,
	sample_index: i64,
	/// Frames handed to the MFT that haven't come back out, oldest first: the
	/// sample time we gave the codec paired with the frame's real timestamp. This
	/// MFT buffers (output for frame N typically arrives while N+1 goes in), so its
	/// output has to be matched against this rather than stamped with whatever is
	/// being fed at the time.
	pending: VecDeque<(i64, Timestamp)>,
	/// The last timestamp paired with an output, reused if the MFT ever hands back
	/// more access units than we fed it frames. Repeating a time is far kinder to a
	/// consumer than the jump to zero the alternative would produce.
	last_timestamp: Option<Timestamp>,
	/// Kept alive for the MFT's lifetime once a texture frame binds a device.
	_manager: Option<IMFDXGIDeviceManager>,
	_com: ComGuard,
}

// The MFT and its COM handles are created, driven, and dropped only on the
// dedicated encode thread (see `encode::sink`), so the per-thread COM apartment
// this opens in `ComGuard::new` stays balanced. `Send` lets the boxed trait
// object satisfy `Backend: Send`.
unsafe impl Send for MediaFoundation {}

impl MediaFoundation {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let format = OutputFormat::for_codec(config.codec);
		let com = ComGuard::new()?;
		let transform = enumerate_encoder(format.subtype)?;

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
			codec = format.label,
			width = config.width,
			height = config.height,
			"opened encoder"
		);
		Ok(Box::new(Self {
			transform,
			events,
			codec_api,
			codec: config.codec,
			width: config.width,
			height: config.height,
			framerate: config.framerate,
			bitrate: clamp_u32(config.resolved_bitrate()),
			gop: config.gop,
			color: config.resolved_color(),
			started: false,
			provides_samples: false,
			output_size: 0,
			needs_input: false,
			drained: false,
			sample_index: 0,
			pending: VecDeque::new(),
			last_timestamp: None,
			_manager: None,
			_com: com,
		}))
	}

	/// One-time configuration, deferred to the first frame so a texture frame can
	/// bind its own Direct3D11 device for zero-copy input.
	fn start(&mut self, frame: &Surface) -> Result<(), Error> {
		// Bind the frame's D3D11 device when it's a texture, so the MFT reads the
		// captured surface directly. A CPU frame runs the MFT in system memory.
		if let Surface::Texture(texture) = frame {
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
		self.read_output_info()?;

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

	/// Cache whether the MFT provides its own output samples and, if not, how
	/// big a buffer we must allocate. Re-read after a format renegotiation.
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

	/// Stamp the color space onto a media type. Media Foundation takes these as a
	/// request rather than a guarantee, so a driver may still emit an untagged
	/// stream; the conversion picks the matrix an untagged stream implies, which
	/// is what keeps that case correct.
	fn set_color(&self, media: &IMFMediaType) -> Result<(), Error> {
		let (primaries, matrix) = match self.color {
			Color::Bt601Limited | Color::Bt601Full => (MFVideoPrimaries_SMPTE170M, MFVideoTransferMatrix_BT601),
			Color::Bt709Limited | Color::Bt709Full => (MFVideoPrimaries_BT709, MFVideoTransferMatrix_BT709),
		};
		// BT.601 and BT.709 share a transfer curve; only primaries and matrix differ.
		let transfer = MFVideoTransFunc_709;
		let range = match self.color.limited() {
			true => MFNominalRange_16_235,
			false => MFNominalRange_0_255,
		};

		unsafe {
			media
				.SetUINT32(&MF_MT_VIDEO_PRIMARIES, primaries.0 as u32)
				.map_err(|e| mf_err("color primaries", e))?;
			media
				.SetUINT32(&MF_MT_TRANSFER_FUNCTION, transfer.0 as u32)
				.map_err(|e| mf_err("transfer function", e))?;
			media
				.SetUINT32(&MF_MT_YUV_MATRIX, matrix.0 as u32)
				.map_err(|e| mf_err("yuv matrix", e))?;
			media
				.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, range.0 as u32)
				.map_err(|e| mf_err("nominal range", e))?;
		}
		Ok(())
	}

	fn set_output_type(&self) -> Result<(), Error> {
		let format = OutputFormat::for_codec(self.codec);
		let media = unsafe { MFCreateMediaType().map_err(|e| mf_err("create output type", e))? };
		unsafe {
			media
				.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
				.map_err(|e| mf_err("output major type", e))?;
			media
				.SetGUID(&MF_MT_SUBTYPE, &format.subtype)
				.map_err(|e| mf_err("output subtype", e))?;
			media
				.SetUINT32(&MF_MT_AVG_BITRATE, self.bitrate)
				.map_err(|e| mf_err("output bitrate", e))?;
			media
				.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
				.map_err(|e| mf_err("output interlace", e))?;
			media
				.SetUINT32(&MF_MT_MPEG2_PROFILE, format.profile)
				.map_err(|e| mf_err("output profile", e))?;
			media
				.SetUINT64(&MF_MT_FRAME_SIZE, pack_2x32(self.width, self.height))
				.map_err(|e| mf_err("output frame size", e))?;
			media
				.SetUINT64(&MF_MT_FRAME_RATE, pack_2x32(self.framerate, 1))
				.map_err(|e| mf_err("output frame rate", e))?;
			self.set_color(&media)?;
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
			self.set_color(&media)?;
			self.transform
				.SetInputType(0, &media, 0)
				.map_err(|e| mf_err("SetInputType", e))?;
		}
		Ok(())
	}

	/// Wrap a captured frame as an input [`IMFSample`]: a zero-copy DXGI surface
	/// buffer for a texture, or a freshly uploaded NV12 memory buffer for I420.
	fn build_sample(&self, frame: &Surface) -> Result<IMFSample, Error> {
		let buffer = match frame {
			Surface::Texture(texture) => unsafe {
				let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, &texture.texture, 0, false)
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
			Surface::I420(_) => self.upload_nv12(frame)?,
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
			sample
				.SetSampleTime(self.sample_time())
				.map_err(|e| mf_err("SetSampleTime", e))?;
			sample
				.SetSampleDuration(self.tick())
				.map_err(|e| mf_err("SetSampleDuration", e))?;
		}
		Ok(sample)
	}

	/// One frame's worth of the Media Foundation sample clock, in 100ns units.
	fn tick(&self) -> i64 {
		HNS_PER_SEC / self.framerate.max(1) as i64
	}

	/// The sample time for the frame currently going in.
	fn sample_time(&self) -> i64 {
		self.sample_index * self.tick()
	}

	/// Copy a CPU I420 frame into a system-memory NV12 buffer (the fallback when
	/// capture isn't producing GPU textures).
	fn upload_nv12(&self, frame: &Surface) -> Result<IMFMediaBuffer, Error> {
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
		// SAFETY: we wrote `len` bytes' worth via the slice below and hold the lock
		// until `Unlock`.
		let nv12 = unsafe { std::slice::from_raw_parts_mut(ptr_out, len) };
		// Y plane verbatim, then interleave U/V into the NV12 chroma plane.
		let (y_dst, uv_dst) = nv12.split_at_mut(w * h);
		y_dst.copy_from_slice(i420.y());
		interleave_uv(i420.u(), i420.v(), uv_dst);
		unsafe {
			let _ = buffer.Unlock();
			buffer
				.SetCurrentLength(len as u32)
				.map_err(|e| mf_err("set NV12 length", e))?;
		}
		Ok(buffer)
	}

	/// Block on events until the MFT is ready for input, collecting any output
	/// that arrives meanwhile. Runs on the dedicated encode thread (see
	/// `encode::sink`), so this blocking wait never parks a tokio worker.
	fn wait_for_input(&mut self, out: &mut Vec<Encoded>) -> Result<(), Error> {
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
	fn drain_ready(&mut self, out: &mut Vec<Encoded>) -> Result<(), Error> {
		loop {
			match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
				Ok(event) => self.handle_event(&event, out)?,
				Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
				Err(e) => return Err(mf_err("GetEvent (drain)", e)),
			}
		}
	}

	fn handle_event(&mut self, event: &IMFMediaEvent, out: &mut Vec<Encoded>) -> Result<(), Error> {
		// Surface an async failure (e.g. an MEError event) instead of looping in
		// `wait_for_input` forever for an input request that will never come.
		unsafe { event.GetStatus() }
			.map_err(|e| mf_err("event GetStatus", e))?
			.ok()
			.map_err(|e| mf_err("encoder reported a failed event", e))?;

		// `GetType` returns the raw `MF_EVENT_TYPE` value as a u32.
		const NEED_INPUT: u32 = METransformNeedInput.0 as u32;
		const HAVE_OUTPUT: u32 = METransformHaveOutput.0 as u32;
		const DRAIN_COMPLETE: u32 = METransformDrainComplete.0 as u32;
		match unsafe { event.GetType().map_err(|e| mf_err("event GetType", e))? } {
			NEED_INPUT => self.needs_input = true,
			HAVE_OUTPUT => {
				if let Some(packet) = self.process_output()? {
					out.push(packet);
				}
			}
			DRAIN_COMPLETE => self.drained = true,
			_ => {}
		}
		Ok(())
	}

	/// Pull one encoded access unit, stamped with the timestamp of the frame it was
	/// encoded from. Returns `None` if the MFT had nothing ready or asked us to
	/// renegotiate the output type.
	fn process_output(&mut self) -> Result<Option<Encoded>, Error> {
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

		// Take ownership of whatever sample slot now holds (ours or the MFT's),
		// and release any event collection the MFT attached.
		let sample = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pSample) });
		let _events = ManuallyDrop::into_inner(unsafe { ptr::read(&data[0].pEvents) });

		match result {
			Ok(()) => {}
			Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
			Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
				// The encoder revised its output format; re-apply ours, refresh the
				// cached output-buffer info, and retry on the next event.
				self.set_output_type()?;
				self.read_output_info()?;
				return Ok(None);
			}
			Err(e) => return Err(mf_err("ProcessOutput", e)),
		}

		let Some(sample) = sample else { return Ok(None) };
		let sample_time = unsafe { sample.GetSampleTime() }.ok();
		Ok(Some(Encoded::new(
			sample_to_bytes(&sample)?,
			self.take_timestamp(sample_time),
		)))
	}

	/// End the stream and wait the MFT's tail out, returning everything it was
	/// still holding.
	///
	/// The blocking wait is the point. A hardware encoder holds a frame or two, and
	/// the events carrying them are posted *after* the drain command, so sweeping
	/// whatever happens to be queued already truncates the stream: that is one
	/// access unit lost per group, and on a live track the frame does not vanish
	/// but reappears ahead of the next group's keyframe.
	///
	/// An async MFT owes a `METransformDrainComplete` for every drain, and a failed
	/// event surfaces through [`handle_event`](Self::handle_event) as an error
	/// rather than a silence, so the wait terminates either way. Both messages that
	/// set that up are checked for the same reason: the wait is only bounded by an
	/// MFT that accepted the drain, so failing to reach that state has to be an
	/// error here rather than a block later.
	fn drain(&mut self) -> Result<Vec<Encoded>, Error> {
		let mut out = Vec::new();
		if !self.started {
			return Ok(out);
		}

		// Cleared here rather than after the wait: a drain we exited early once
		// leaves its completion queued, and reading that later would let the next
		// drain believe it was already done.
		self.drained = false;
		unsafe {
			self.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
				.map_err(|e| mf_err("end of stream", e))?;
			self.transform
				.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
				.map_err(|e| mf_err("drain", e))?;
		}
		while !self.drained {
			let event =
				unsafe { self.events.GetEvent(MF_EVENT_FLAG_NONE) }.map_err(|e| mf_err("GetEvent (drain)", e))?;
			self.handle_event(&event, &mut out)?;
		}
		Ok(out)
	}

	/// The timestamp of the frame this output belongs to, found by the sample time
	/// the MFT echoed back and removed from `pending`.
	///
	/// Falls back to the oldest frame still outstanding when the MFT reports no
	/// usable sample time: the encoder emits access units in the order it was fed
	/// (low-latency CBR, no reordering), so oldest-first is the right pairing, and
	/// dropping the packet or stamping it with the wrong frame would both be worse.
	fn take_timestamp(&mut self, sample_time: Option<i64>) -> Timestamp {
		let found = sample_time.and_then(|time| self.pending.iter().position(|(fed, _)| *fed == time));

		let matched = match found {
			// `remove` rather than `pop_front`: with reordering off these coincide, but
			// a reordering MFT would otherwise pair every later packet wrongly.
			Some(index) => self.pending.remove(index),
			None => {
				let oldest = self.pending.pop_front();
				if oldest.is_some() {
					tracing::debug!(?sample_time, "encoder output did not match a fed sample time");
				}
				oldest
			}
		};

		match matched {
			Some((_, timestamp)) => {
				self.last_timestamp = Some(timestamp);
				timestamp
			}
			// Nothing outstanding: the MFT produced more access units than we fed it
			// frames, which shouldn't happen. Repeat the last frame's time so the
			// stream keeps flowing in order rather than jumping backwards.
			None => {
				tracing::warn!("encoder produced output with no frame outstanding");
				self.last_timestamp.unwrap_or(Timestamp::ZERO)
			}
		}
	}
}

impl Backend for MediaFoundation {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Encoded>, Error> {
		if !self.started {
			self.start(&frame.surface)?;
		}

		let mut out = Vec::new();
		self.wait_for_input(&mut out)?;

		if keyframe {
			self.set_codec(&CODECAPI_AVEncVideoForceKeyFrame, variant_u32(1))?;
		}

		let sample = self.build_sample(&frame.surface)?;
		unsafe {
			self.transform
				.ProcessInput(0, &sample, 0)
				.map_err(|e| mf_err("ProcessInput", e))?;
		}
		self.needs_input = false;
		// Record the pairing before advancing the index, so this is the sample time
		// `build_sample` just stamped the sample with.
		let sample_time = self.sample_time();
		self.pending.push_back((sample_time, frame.timestamp));
		self.sample_index += 1;

		self.drain_ready(&mut out)?;
		Ok(out)
	}

	fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
		let out = self.drain()?;
		if self.started {
			// A drain leaves the MFT stopped: it stops asking for input until a new
			// stream starts.
			unsafe {
				self.transform
					.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
					.map_err(|e| mf_err("start of stream", e))?;
			}
			self.needs_input = false;
		}
		Ok(out)
	}

	fn finish(&mut self) -> Result<Vec<Encoded>, Error> {
		self.drain()
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		let bitrate = clamp_u32(bitrate);

		// Not `set_codec`: that swallows failures because the knobs it sets at
		// open are advisory, whereas an MFT refusing a rate change means the
		// control loop is talking to itself and should hear about it.
		let value = variant_u32(bitrate);
		unsafe { self.codec_api.SetValue(&CODECAPI_AVEncCommonMeanBitRate, &value) }
			.map_err(|e| mf_err(&format!("set bitrate to {bitrate}"), e))?;

		// Keep the cache in step: a later format renegotiation rebuilds the
		// output type from it, and it would otherwise reinstate the old rate.
		self.bitrate = bitrate;
		Ok(())
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// Pick the first hardware encoder MFT (NV12 in, `subtype` out).
fn enumerate_encoder(subtype: GUID) -> Result<IMFTransform, Error> {
	let input = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: MFVideoFormat_NV12,
	};
	let output = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: subtype,
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
		return Err(Error::Codec(anyhow::anyhow!("no hardware encoder found")));
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
		windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const std::ffi::c_void));
	}

	transform.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to activate encoder MFT")))
}

/// The Media Foundation output type for a codec: the format subtype enumerated
/// and set on the MFT, plus the `MF_MT_MPEG2_PROFILE` value (the attribute MF
/// reuses to carry the H.264/H.265 profile).
struct OutputFormat {
	subtype: GUID,
	profile: u32,
	label: &'static str,
}

impl OutputFormat {
	fn for_codec(codec: Codec) -> Self {
		match codec {
			Codec::H264 => Self {
				subtype: MFVideoFormat_H264,
				profile: eAVEncH264VProfile_High.0 as u32,
				label: "H.264",
			},
			Codec::H265 => Self {
				subtype: MFVideoFormat_HEVC,
				profile: eAVEncH265VProfile_Main_420_8.0 as u32,
				label: "H.265",
			},
		}
	}
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
