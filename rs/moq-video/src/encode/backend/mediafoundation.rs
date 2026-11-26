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
//!
//! Rate control is settled at open from what the MFT lists, and reported in
//! [`Applied`]. An MFT that lists peak-constrained VBR runs it with the bitrate
//! as both the mean and the leaky bucket's drain rate, so a busy scene is held
//! to the cap; any other runs CBR at the cap. Either way a quantizer floor
//! ([`MIN_QP`]), where the MFT supports one, is the quality: a calm scene the
//! floor already codes cleanly spends less than the cap. Microsoft's own H.265
//! MFT, for one, runs CBR when asked for a mode it lacks, so what the MFT lists
//! decides, not whether setting the mode succeeds.

use std::collections::VecDeque;
use std::mem::ManuallyDrop;
use std::ptr;

use bytes::Bytes;
use moq_net::Timestamp;
use windows::Win32::Foundation::{VARIANT_BOOL, VARIANT_TRUE};
use windows::Win32::Graphics::Direct3D11::{ID3D11Device, ID3D11Texture2D};
use windows::Win32::Media::MediaFoundation::{
	CODECAPI_AVEncCommonBufferSize, CODECAPI_AVEncCommonMaxBitRate, CODECAPI_AVEncCommonMeanBitRate,
	CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame,
	CODECAPI_AVEncVideoMinQP, CODECAPI_AVLowLatencyMode, ICodecAPI, IMFDXGIDeviceManager, IMFMediaBuffer,
	IMFMediaEvent, IMFMediaEventGenerator, IMFMediaType, IMFSample, IMFTransform, METransformDrainComplete,
	METransformHaveOutput, METransformNeedInput, MF_E_NO_EVENTS_AVAILABLE, MF_E_TRANSFORM_NEED_MORE_INPUT,
	MF_E_TRANSFORM_STREAM_CHANGE, MF_EVENT_FLAG_NO_WAIT, MF_EVENT_FLAG_NONE, MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE,
	MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG2_PROFILE, MF_MT_SUBTYPE,
	MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX,
	MF_TRANSFORM_ASYNC_UNLOCK, MFCreateDXGIDeviceManager, MFCreateDXGISurfaceBuffer, MFCreateMediaType,
	MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video, MFNominalRange_0_255, MFNominalRange_16_235,
	MFSampleExtension_Discontinuity, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_HARDWARE, MFT_ENUM_FLAG_SORTANDFILTER,
	MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
	MFT_MESSAGE_NOTIFY_START_OF_STREAM, MFT_MESSAGE_SET_D3D_MANAGER, MFT_OUTPUT_DATA_BUFFER,
	MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264, MFVideoFormat_HEVC,
	MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoPrimaries_BT2020,
	MFVideoPrimaries_EBU3213, MFVideoPrimaries_SMPTE170M, MFVideoTransFunc_709, MFVideoTransFunc_2084,
	MFVideoTransFunc_HLG, MFVideoTransferMatrix_BT601, MFVideoTransferMatrix_BT709, MFVideoTransferMatrix_BT2020_10,
	eAVEncCommonRateControlMode_CBR, eAVEncCommonRateControlMode_PeakConstrainedVBR, eAVEncH264VProfile_High,
	eAVEncH265VProfile_Main_420_8,
};
use windows::Win32::System::Variant::{VARIANT, VT_BOOL, VT_I4, VT_UI4, VariantClear};
use windows::core::{GUID, Interface};

use super::super::encoder::{Applied, Codec, Config};
use super::{Backend, Encoded, keyframe_annexb};
use crate::color::{Matrix, Primaries, Range, Transfer};
use crate::frame::{Surface, interleave_uv};
use crate::mf::{ComGuard, Locked, memory_buffer, mf_err, pack_2x32};
use crate::{Color, Error, Frame};

pub(crate) const NAME: &str = "mediafoundation";

/// Stream tick for sample timestamps, in 100ns units (the Media Foundation time
/// base). The codec only needs a clock that increases, so a monotonic index over
/// the framerate is enough; the moq timestamp rides alongside in `pending` and the
/// MFT echoes the sample time on its output, which is what pairs the two back up.
const HNS_PER_SEC: i64 = 10_000_000;

/// The quantizer floor, which is the quality: the coarsest quantizer that kept
/// a 720p30 talking head at VMAF 95, the usual mark for transparent, on the
/// Intel and AMD encoders measured through VA-API (`moq_vaapi`'s `MIN_QP`).
/// The same silicon sits behind their Media Foundation MFTs.
const MIN_QP: u32 = 24;

/// The rate controller's buffer, in frames of the bitrate. A second of buffer
/// lets a busy second after a still one run that much over the cap; three
/// frames is what VA-API and NVENC hold to.
const BUFFER_FRAMES: u32 = 3;

pub(crate) struct MediaFoundation {
	transform: IMFTransform,
	events: IMFMediaEventGenerator,
	codec_api: ICodecAPI,
	codec: Codec,
	width: u32,
	height: u32,
	framerate: crate::FrameRate,
	bitrate: u32,
	/// How the MFT holds the stream to `bitrate`, settled at open.
	rate: Rate,
	/// Keyframe interval in frames.
	gop: u32,
	/// The color space of the input frames, stamped onto both media types so the
	/// encoder writes it into the bitstream's VUI.
	color: Color,
	phase: Phase,
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
	/// Access units the MFT handed back and `receive` has not popped yet.
	ready: VecDeque<Encoded>,
	/// Kept alive for the MFT's lifetime once a texture frame binds a device.
	_manager: Option<IMFDXGIDeviceManager>,
	_com: ComGuard,
}

/// Where the MFT stands.
enum Phase {
	/// Not configured yet: that happens on the first frame, since the
	/// Direct3D11 device to bind (for zero-copy texture input) comes from the
	/// frame itself.
	Unstarted,
	Streaming(Stream),
	/// The MFT failed to start, drain, or restart, so it takes no more frames.
	Ended,
}

/// What a streaming MFT needs between calls.
struct Stream {
	output: Output,
	/// True once the MFT has asked for input and we haven't fed it since.
	needs_input: bool,
	/// Set while the next sample is the first of a restarted stream, so it can be
	/// marked as such.
	discontinuity: bool,
}

/// Who allocates the output samples, cached from `GetOutputStreamInfo` so the
/// output hot path doesn't re-query.
#[derive(Clone, Copy, Debug)]
enum Output {
	/// The MFT allocates its own (true for hardware encoders).
	Provided,
	/// We allocate them, this many bytes each.
	Allocated(u32),
}

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
		let rate = Rate::probe(&codec_api);

		tracing::info!(
			encoder = NAME,
			codec = format.label,
			width = config.width,
			height = config.height,
			%rate,
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
			bitrate: clamp_u32(config.resolved_bitrate().as_bps()),
			rate,
			gop: config.keyframe_interval(),
			color: config.resolved_color(),
			phase: Phase::Unstarted,
			sample_index: 0,
			pending: VecDeque::new(),
			last_timestamp: None,
			ready: VecDeque::new(),
			_manager: None,
			_com: com,
		}))
	}

	/// One-time configuration, deferred to the first frame so a texture frame can
	/// bind its own Direct3D11 device for zero-copy input.
	///
	/// A start that fails leaves the encoder [`Phase::Ended`]: the MFT is
	/// configured partway, which nothing here can take back.
	fn start(&mut self, frame: &Surface) -> Result<(), Error> {
		self.phase = Phase::Ended;
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
		let output = self.read_output_info()?;

		unsafe {
			self.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
				.map_err(|e| mf_err("begin streaming", e))?;
			self.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
				.map_err(|e| mf_err("start of stream", e))?;
		}
		self.phase = Phase::Streaming(Stream {
			output,
			needs_input: false,
			discontinuity: false,
		});
		Ok(())
	}

	/// Whether the MFT provides its own output samples and, if not, how big a
	/// buffer we must allocate. Re-read after a format renegotiation.
	fn read_output_info(&self) -> Result<Output, Error> {
		let info = unsafe {
			self.transform
				.GetOutputStreamInfo(0)
				.map_err(|e| mf_err("GetOutputStreamInfo", e))?
		};
		Ok(match info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0 {
			true => Output::Provided,
			false => Output::Allocated(info.cbSize),
		})
	}

	/// The stream's state, which only a started encoder has.
	fn stream(&mut self) -> Result<&mut Stream, Error> {
		match &mut self.phase {
			Phase::Streaming(stream) => Ok(stream),
			Phase::Unstarted => Err(Error::codec(anyhow::anyhow!(
				"the mediafoundation encoder has not started"
			))),
			Phase::Ended => Err(Error::codec(anyhow::anyhow!(
				"the mediafoundation encoder has ended its stream and takes no more frames"
			))),
		}
	}

	/// Set the codec properties before the media types, which is when the MFT
	/// reads the static ones such as the rate control mode.
	fn configure_codec_api(&self) -> Result<(), Error> {
		// Low latency: no B-frames / lookahead, so output tracks input closely.
		self.set_codec(&CODECAPI_AVLowLatencyMode, variant_bool(true))?;
		self.set_codec(&CODECAPI_AVEncMPVGOPSize, variant_u32(self.gop))?;

		// The rate controls the MFT advertised at open: refusing one now would
		// leave the stream on a rate control [`Applied`] does not report.
		let mode = match self.rate.mode {
			Mode::Capped => eAVEncCommonRateControlMode_PeakConstrainedVBR,
			Mode::Constant => eAVEncCommonRateControlMode_CBR,
		};
		self.require(&CODECAPI_AVEncCommonRateControlMode, mode.0 as u32, "rate control mode")?;
		self.set_rate(self.bitrate, None)?;
		if self.rate.floor {
			self.require(&CODECAPI_AVEncVideoMinQP, MIN_QP, "quantizer floor")?;
		}
		Ok(())
	}

	/// Hold the stream to `bitrate`, moving from `from` (`None` before the
	/// stream starts): the mean, the peak in peak-constrained VBR, and the
	/// buffer. A raise sets the peak first and a cut sets the mean first, so the
	/// mean never passes the peak in between.
	fn set_rate(&self, bitrate: u32, from: Option<u32>) -> Result<(), Error> {
		let mean = || self.require(&CODECAPI_AVEncCommonMeanBitRate, bitrate, "mean bitrate");
		let peak = || match self.rate.mode {
			Mode::Capped => self.require(&CODECAPI_AVEncCommonMaxBitRate, bitrate, "peak bitrate"),
			Mode::Constant => Ok(()),
		};
		match from.is_some_and(|from| bitrate < from) {
			true => mean().and_then(|()| peak())?,
			false => peak().and_then(|()| mean())?,
		}
		if self.rate.buffer {
			self.require(
				&CODECAPI_AVEncCommonBufferSize,
				buffer_bytes(bitrate, self.framerate),
				"rate control buffer",
			)?;
		}
		Ok(())
	}

	/// Set a codec property the encoder cannot run correctly without.
	fn require(&self, api: &GUID, value: u32, what: &str) -> Result<(), Error> {
		unsafe { self.codec_api.SetValue(api, &variant_u32(value)) }
			.map_err(|e| mf_err(&format!("set the {what} to {value}"), e))
	}

	fn set_codec(&self, api: *const windows::core::GUID, value: VARIANT) -> Result<(), Error> {
		// Low latency and the GOP are advisory: the stream is still correct
		// without them, so a refusal is only logged.
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
		let primaries = match self.color.primaries {
			Primaries::Smpte170m => MFVideoPrimaries_SMPTE170M,
			Primaries::Bt470bg => MFVideoPrimaries_EBU3213,
			Primaries::Bt709 => MFVideoPrimaries_BT709,
			Primaries::Bt2020 => MFVideoPrimaries_BT2020,
		};
		let transfer = match self.color.transfer {
			Transfer::Bt709 => MFVideoTransFunc_709,
			Transfer::Pq => MFVideoTransFunc_2084,
			Transfer::Hlg => MFVideoTransFunc_HLG,
		};
		let matrix = match self.color.matrix {
			Matrix::Bt601 => MFVideoTransferMatrix_BT601,
			Matrix::Bt709 => MFVideoTransferMatrix_BT709,
			Matrix::Bt2020 => MFVideoTransferMatrix_BT2020_10,
		};
		let range = match self.color.range {
			Range::Limited => MFNominalRange_16_235,
			Range::Full => MFNominalRange_0_255,
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
				.SetUINT64(
					&MF_MT_FRAME_RATE,
					pack_2x32(self.framerate.numerator(), self.framerate.denominator()),
				)
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
				.SetUINT64(
					&MF_MT_FRAME_RATE,
					pack_2x32(self.framerate.numerator(), self.framerate.denominator()),
				)
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
			#[allow(
				unreachable_patterns,
				reason = "the other surfaces exist only on some platforms and features"
			)]
			_ => {
				return Err(Error::codec(anyhow::anyhow!(
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
		HNS_PER_SEC * i64::from(self.framerate.denominator()) / i64::from(self.framerate.numerator())
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

		let buffer = memory_buffer(len)?;
		{
			let mut locked = Locked::new(&buffer)?;
			// Y plane verbatim, then interleave U/V into the NV12 chroma plane.
			let (y_dst, uv_dst) = locked.data_mut(len)?.split_at_mut(w * h);
			y_dst.copy_from_slice(i420.y());
			interleave_uv(i420.u(), i420.v(), uv_dst);
		}
		unsafe {
			// `memory_buffer` already refused a length past `u32`.
			buffer
				.SetCurrentLength(len as u32)
				.map_err(|e| mf_err("set NV12 length", e))?;
		}
		Ok(buffer)
	}

	/// Block on events until the MFT is ready for input, queueing any output
	/// that arrives meanwhile. Runs on the dedicated encode thread (see
	/// `encode::sink`), so this blocking wait never parks a tokio worker.
	fn wait_for_input(&mut self) -> Result<(), Error> {
		while !self.stream()?.needs_input {
			let event = unsafe {
				self.events
					.GetEvent(MF_EVENT_FLAG_NONE)
					.map_err(|e| mf_err("GetEvent", e))?
			};
			self.handle_event(&event)?;
		}
		Ok(())
	}

	/// Drain events already queued without blocking (called after feeding input).
	fn drain_ready(&mut self) -> Result<(), Error> {
		loop {
			match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
				Ok(event) => {
					self.handle_event(&event)?;
				}
				Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
				Err(e) => return Err(mf_err("GetEvent (drain)", e)),
			}
		}
	}

	/// Act on one event from the MFT, returning whether it reported a drain
	/// complete, which is how a drain knows the tail is out rather than still
	/// coming.
	fn handle_event(&mut self, event: &IMFMediaEvent) -> Result<bool, Error> {
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
			NEED_INPUT => self.stream()?.needs_input = true,
			HAVE_OUTPUT => {
				if let Some(packet) = self.process_output()? {
					self.ready.push_back(packet);
				}
			}
			DRAIN_COMPLETE => return Ok(true),
			_ => {}
		}
		Ok(false)
	}

	/// Pull one encoded access unit, stamped with the timestamp of the frame it was
	/// encoded from. Returns `None` if the MFT had nothing ready or asked us to
	/// renegotiate the output type.
	fn process_output(&mut self) -> Result<Option<Encoded>, Error> {
		let provided = match self.stream()?.output {
			Output::Provided => None,
			Output::Allocated(size) => {
				let buffer = unsafe { MFCreateMemoryBuffer(size).map_err(|e| mf_err("output buffer", e))? };
				let sample = unsafe { MFCreateSample().map_err(|e| mf_err("output sample", e))? };
				unsafe { sample.AddBuffer(&buffer).map_err(|e| mf_err("output AddBuffer", e))? };
				Some(sample)
			}
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
				self.stream()?.output = self.read_output_info()?;
				return Ok(None);
			}
			Err(e) => return Err(mf_err("ProcessOutput", e)),
		}

		let Some(sample) = sample else { return Ok(None) };
		let sample_time = unsafe { sample.GetSampleTime() }.ok();
		let payload = sample_to_bytes(&sample)?;
		// Read from the bitstream rather than `MFSampleExtension_CleanPoint`, which a
		// hardware MFT is not required to set.
		let keyframe = keyframe_annexb(self.codec, &payload);
		Ok(Some(Encoded::new(payload, self.take_timestamp(sample_time), keyframe)))
	}

	/// End the stream and wait the MFT's tail out, queueing everything it was
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
	fn drain(&mut self) -> Result<(), Error> {
		unsafe {
			self.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
				.map_err(|e| mf_err("end of stream", e))?;
			self.transform
				.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
				.map_err(|e| mf_err("drain", e))?;
		}
		loop {
			let event =
				unsafe { self.events.GetEvent(MF_EVENT_FLAG_NONE) }.map_err(|e| mf_err("GetEvent (drain)", e))?;
			if self.handle_event(&event)? {
				return Ok(());
			}
		}
	}

	/// Drain the stream and restart it, or end the encoder for good.
	///
	/// A drain cut short leaves the MFT somewhere between streams, with its
	/// completion perhaps still queued for the next drain to mistake for its own,
	/// so nothing after one can be trusted.
	fn cycle(&mut self) -> Result<(), Error> {
		let cycled = self.drain().and_then(|()| {
			// A drain leaves the MFT stopped: it stops asking for input until a new
			// stream starts.
			unsafe {
				self.transform
					.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
					.map_err(|e| mf_err("start of stream", e))
			}
		});
		match cycled {
			Ok(()) => {
				let stream = self.stream()?;
				stream.needs_input = false;
				stream.discontinuity = true;
				Ok(())
			}
			Err(err) => {
				self.phase = Phase::Ended;
				Err(err)
			}
		}
	}

	/// The timestamp of the frame this output belongs to, found by the sample time
	/// the MFT echoed back and removed from `pending`.
	///
	/// Falls back to the oldest frame still outstanding when the MFT reports no
	/// usable sample time: the encoder emits access units in the order it was fed
	/// (low latency, no reordering), so oldest-first is the right pairing, and
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
	fn encode(&mut self, frame: &Frame, cut: bool) -> Result<(), Error> {
		if let Phase::Unstarted = self.phase {
			self.start(&frame.surface)?;
		}

		self.wait_for_input()?;

		if cut {
			self.set_codec(&CODECAPI_AVEncVideoForceKeyFrame, variant_u32(1))?;
		}

		let sample = self.build_sample(&frame.surface)?;
		if self.stream()?.discontinuity {
			// The first picture of a restarted stream is not temporally continuous
			// with the one before the drain, and only this says so: the encoder would
			// otherwise be free to predict across the seam.
			unsafe { sample.SetUINT32(&MFSampleExtension_Discontinuity, 1) }
				.map_err(|e| mf_err("mark discontinuity", e))?;
		}
		unsafe {
			self.transform
				.ProcessInput(0, &sample, 0)
				.map_err(|e| mf_err("ProcessInput", e))?;
		}
		// Cleared only once the sample is in: a rejected one never carried the mark
		// anywhere, so the next attempt still owns it.
		let stream = self.stream()?;
		stream.discontinuity = false;
		stream.needs_input = false;
		// Record the pairing before advancing the index, so this is the sample time
		// `build_sample` just stamped the sample with.
		let sample_time = self.sample_time();
		self.pending.push_back((sample_time, frame.timestamp));
		self.sample_index += 1;

		self.drain_ready()
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		if let Phase::Unstarted = self.phase {
			return Ok(());
		}
		// An ended encoder fails here rather than being sent a drain.
		self.stream()?;
		self.cycle()
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		let bitrate = clamp_u32(bitrate);

		// An MFT refusing a rate change means the control loop is talking to
		// itself, and a peak left behind breaks the cap, so either is an error.
		self.set_rate(bitrate, Some(self.bitrate))?;

		// Keep the cache in step: a later format renegotiation rebuilds the
		// output type from it, and it would otherwise reinstate the old rate.
		self.bitrate = bitrate;
		Ok(())
	}

	fn can_cut(&self) -> bool {
		true
	}

	fn name(&self) -> &'static str {
		NAME
	}

	fn applied(&self) -> Applied {
		// `set_codec` only logs a refused knob, and the MFT is configured lazily on
		// the first frame, so nothing confirms low-latency mode took.
		Applied::unconfirmed(format!("AVLowLatencyMode requested, unconfirmed; {}", self.rate))
	}
}

/// How the MFT holds the stream to the cap, settled at open from the controls
/// it advertises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rate {
	mode: Mode,
	/// Whether the MFT supports a quantizer floor, set at [`MIN_QP`].
	floor: bool,
	/// Whether the MFT supports sizing the buffer, set at [`BUFFER_FRAMES`];
	/// otherwise it keeps its own.
	buffer: bool,
}

/// The rate control mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
	/// Peak-constrained VBR, the bitrate both the mean and the leaky bucket's
	/// drain rate.
	Capped,
	/// CBR at the bitrate, for an MFT that does not list peak-constrained VBR.
	Constant,
}

impl Rate {
	/// Read what the MFT advertises; nothing is set until the stream starts.
	fn probe(codec_api: &ICodecAPI) -> Self {
		let capped = rate_modes(codec_api).contains(&(eAVEncCommonRateControlMode_PeakConstrainedVBR.0 as u32));
		Self {
			mode: if capped { Mode::Capped } else { Mode::Constant },
			floor: unsafe { codec_api.IsSupported(&CODECAPI_AVEncVideoMinQP) }.is_ok(),
			buffer: unsafe { codec_api.IsSupported(&CODECAPI_AVEncCommonBufferSize) }.is_ok(),
		}
	}
}

impl std::fmt::Display for Rate {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self.mode {
			Mode::Capped => f.write_str("peak-constrained VBR at the cap")?,
			Mode::Constant => f.write_str("CBR at the cap")?,
		}
		match self.floor {
			true => write!(f, ", QP floor {MIN_QP}")?,
			false => f.write_str(", no QP floor")?,
		}
		match self.buffer {
			true => write!(f, ", {BUFFER_FRAMES}-frame buffer"),
			false => f.write_str(", the MFT's own buffer"),
		}
	}
}

/// The rate control modes the MFT lists, empty when it lists none (listing is
/// optional for an MFT).
fn rate_modes(codec_api: &ICodecAPI) -> Vec<u32> {
	let mut values: *mut VARIANT = ptr::null_mut();
	let mut count = 0u32;
	if unsafe { codec_api.GetParameterValues(&CODECAPI_AVEncCommonRateControlMode, &mut values, &mut count) }.is_err()
		|| values.is_null()
	{
		return Vec::new();
	}
	// SAFETY: the MFT handed back `count` VARIANTs in one CoTaskMemAlloc block,
	// which we own: each is cleared, then the block freed.
	let entries = unsafe { std::slice::from_raw_parts_mut(values, count as usize) };
	let modes = entries
		.iter_mut()
		.filter_map(|value| {
			let inner = unsafe { &value.Anonymous.Anonymous };
			let mode = match inner.vt {
				VT_UI4 => Some(unsafe { inner.Anonymous.ulVal }),
				VT_I4 => u32::try_from(unsafe { inner.Anonymous.lVal }).ok(),
				_ => None,
			};
			// Clearing an integer frees nothing, so a failure leaks nothing either.
			let _ = unsafe { VariantClear(value) };
			mode
		})
		.collect();
	unsafe {
		windows::Win32::System::Com::CoTaskMemFree(Some(values as *const std::ffi::c_void));
	}
	modes
}

/// [`BUFFER_FRAMES`] of `bitrate`, in bytes, the unit Media Foundation's H.264
/// and H.265 encoders take.
fn buffer_bytes(bitrate: u32, framerate: crate::FrameRate) -> u32 {
	let bytes = u64::from(bitrate) * u64::from(BUFFER_FRAMES) * u64::from(framerate.denominator())
		/ (8 * u64::from(framerate.numerator().max(1)));
	bytes.clamp(1, u64::from(u32::MAX)) as u32
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
		return Err(Error::codec(anyhow::anyhow!("no hardware encoder found")));
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

	transform.ok_or_else(|| Error::codec(anyhow::anyhow!("failed to activate encoder MFT")))
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
	let manager = manager.ok_or_else(|| Error::codec(anyhow::anyhow!("MFCreateDXGIDeviceManager returned null")))?;
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
	Ok(Bytes::copy_from_slice(Locked::new(&buffer)?.data()))
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

#[cfg(test)]
mod tests {
	use super::super::test_util::{Scene, spend};
	use super::*;

	/// What the first hardware MFT for `codec` advertises, or `None` without one.
	fn advertised(codec: Codec) -> Option<Rate> {
		let _com = ComGuard::new().ok()?;
		let transform = enumerate_encoder(OutputFormat::for_codec(codec).subtype).ok()?;
		Some(Rate::probe(&transform.cast::<ICodecAPI>().ok()?))
	}

	/// A calm scene spends well under a generous cap, held there by the
	/// quantizer floor; a busy one, and a still one turning busy, are held to
	/// the cap plus the buffer, and to a lower cap once `set_bitrate` moves it.
	/// An MFT keeping its own buffer is allowed a second of it. One opening
	/// keyframe, so the seconds measured are rate control's alone. Skips without
	/// a hardware MFT, which GitHub's Windows runners lack.
	#[test]
	fn quality_under_a_cap() {
		for codec in [Codec::H264, Codec::H265] {
			let Some(rate) = advertised(codec) else {
				eprintln!("skipping: no hardware {codec:?} MFT");
				continue;
			};
			eprintln!("{codec:?}: {rate}");
			let mut config = Config::new(640, 360, crate::FrameRate::new(30, 1).unwrap());
			config.kind = crate::encode::Kind::Named(NAME.into());
			config.codec = codec;
			config.join = std::time::Duration::from_secs(10);
			let at = |bps: u64| Config {
				bitrate: Some(moq_net::bandwidth::Rate::from_bps(bps)),
				..config.clone()
			};
			let ceiling = |cap: u64| match rate.buffer {
				true => cap + cap * u64::from(BUFFER_FRAMES) / 30,
				false => 2 * cap,
			};

			let calm = spend(&at(8_000_000), Scene::Calm, None).unwrap();
			eprintln!("{codec:?} calm under 8 Mbit/s: {calm:?}");
			if rate.floor {
				assert!(
					calm.mean < 8_000_000 / 3,
					"{codec:?}: a calm scene filled the cap: {calm:?}"
				);
			}

			let busy = spend(&at(2_000_000), Scene::Busy, None).unwrap();
			eprintln!("{codec:?} busy under 2 Mbit/s: {busy:?}");
			assert!(
				busy.mean > 1_000_000,
				"{codec:?}: a busy scene fell short of the cap: {busy:?}"
			);
			assert!(
				busy.peak <= ceiling(2_000_000),
				"{codec:?}: a busy scene broke the cap: {busy:?}"
			);

			let waking = spend(&at(2_000_000), Scene::Waking, None).unwrap();
			eprintln!("{codec:?} still turning busy under 2 Mbit/s: {waking:?}");
			assert!(
				waking.peak <= ceiling(2_000_000),
				"{codec:?}: a still scene turning busy broke the cap: {waking:?}"
			);

			let moved = spend(&at(2_000_000), Scene::Busy, Some(1_000_000)).unwrap();
			eprintln!("{codec:?} busy, cap moved to 1 Mbit/s: {moved:?}");
			assert!(
				moved.peak <= ceiling(1_000_000),
				"{codec:?}: the cap did not follow set_bitrate: {moved:?}"
			);
		}
	}
}
