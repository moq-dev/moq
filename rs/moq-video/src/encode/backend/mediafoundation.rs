//! Hardware H.264 backend via a Media Foundation Transform (MFT).
//!
//! Enumerates the system's hardware H.264 encoder MFTs (Intel QSV / NVENC / AMD
//! VCE, whichever the GPU exposes) and drives the first that accepts NV12 in and
//! H.264 out. Hardware encoder MFTs are *asynchronous*: they signal readiness
//! through an [`IMFMediaEventGenerator`] (`METransformNeedInput` /
//! `METransformHaveOutput`) rather than the synchronous `ProcessInput` /
//! `ProcessOutput` contract. We pump that event queue from the single
//! capture/encode thread, so no separate worker or callback is needed.
//!
//! Low-latency mode is requested via `ICodecAPI` so the encoder runs 1-in-1-out
//! (no B-frames, no lookahead); output then trails input by at most a frame,
//! which is fine for live. Each output sample is already an Annex-B byte stream;
//! on a keyframe we prepend the cached SPS/PPS so the stream stays self-contained
//! (avc3), matching every other backend.

use std::mem::ManuallyDrop;
use std::ptr;
use std::slice;

use bytes::{BufMut, Bytes, BytesMut};
use windows::Win32::Media::MediaFoundation::{
	CODECAPI_AVEncCommonRateControlMode, CODECAPI_AVEncVideoForceKeyFrame, CODECAPI_AVLowLatencyMode, ICodecAPI,
	IMFActivate, IMFMediaEventGenerator, IMFSample, IMFTransform, MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS,
	METransformDrainComplete, METransformHaveOutput, METransformNeedInput, MF_E_TRANSFORM_NEED_MORE_INPUT,
	MF_MT_AVG_BITRATE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
	MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
	MF_TRANSFORM_ASYNC_UNLOCK, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample, MFMediaType_Video,
	MFSampleExtension_CleanPoint, MFT_CATEGORY_VIDEO_ENCODER, MFT_ENUM_FLAG_ASYNCMFT, MFT_ENUM_FLAG_HARDWARE,
	MFT_ENUM_FLAG_SORTANDFILTER, MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
	MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_MESSAGE_NOTIFY_END_STREAMING, MFT_MESSAGE_NOTIFY_START_OF_STREAM,
	MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_PROVIDES_SAMPLES, MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_H264,
	MFVideoFormat_NV12, MFVideoInterlace_Progressive, eAVEncCommonRateControlMode_CBR, eAVEncH264VProfile_High,
};
use windows::Win32::System::Variant::VARIANT;
use windows::core::Interface;

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::Frame;
use crate::win::{ComGuard, mf_err};

pub(crate) const NAME: &str = "mediafoundation";

/// 100-nanosecond ticks per second: the Media Foundation time unit.
const HNS_PER_SEC: i64 = 10_000_000;

fn pack_2x32(hi: u32, lo: u32) -> u64 {
	((hi as u64) << 32) | lo as u64
}

pub(crate) struct MediaFoundation {
	transform: IMFTransform,
	events: IMFMediaEventGenerator,
	codec_api: Option<ICodecAPI>,
	/// SPS/PPS (Annex-B) from the negotiated output type, prepended on keyframes
	/// when the encoder doesn't already emit them in-band.
	sequence_header: Vec<u8>,
	/// True when the MFT allocates its own output samples (the usual hardware
	/// case); then `ProcessOutput` is called with a null sample.
	provides_samples: bool,
	framerate: u32,
	/// Outstanding `METransformNeedInput` events the MFT has raised but we
	/// haven't yet satisfied with a `ProcessInput`.
	need_input: u32,
	frame_index: i64,
	// Drop last: tear down Media Foundation only after the transform releases.
	_com: ComGuard,
}

// The transform and its event generator are only ever touched from the one
// capture/encode thread (see `publish_capture`'s `spawn_blocking`).
unsafe impl Send for MediaFoundation {}

impl MediaFoundation {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let com = ComGuard::new()?;

		let transform = enumerate_hardware()?;

		// Unlock the async MFT so ProcessInput/ProcessOutput stop returning
		// MF_E_TRANSFORM_ASYNC_LOCKED, then grab its event generator.
		unsafe {
			let attrs = transform
				.GetAttributes()
				.map_err(|e| mf_err("transform attributes", e))?;
			attrs
				.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)
				.map_err(|e| mf_err("async unlock", e))?;
		}
		let events: IMFMediaEventGenerator = transform.cast().map_err(|e| mf_err("event generator", e))?;

		// Optional codec controls: low latency (1-in-1-out) + CBR rate control.
		// Best-effort; a plain MFT without ICodecAPI still encodes correctly.
		let codec_api: Option<ICodecAPI> = transform.cast().ok();
		if let Some(api) = &codec_api {
			let _ = unsafe { api.SetValue(&CODECAPI_AVLowLatencyMode, &VARIANT::from(true)) };
			let _ = unsafe {
				api.SetValue(
					&CODECAPI_AVEncCommonRateControlMode,
					&VARIANT::from(eAVEncCommonRateControlMode_CBR.0),
				)
			};
		}

		// Encoders require the output type set before the input type.
		set_output_type(&transform, config)?;
		set_input_type(&transform, config)?;

		let provides_samples = unsafe {
			let info = transform
				.GetOutputStreamInfo(0)
				.map_err(|e| mf_err("output stream info", e))?;
			info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 != 0
		};

		let sequence_header = read_sequence_header(&transform);

		// Begin streaming. The MFT starts raising METransformNeedInput once it's
		// ready for the first frame.
		unsafe {
			transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)
				.map_err(|e| mf_err("begin streaming", e))?;
			transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)
				.map_err(|e| mf_err("start of stream", e))?;
		}

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
			sequence_header,
			provides_samples,
			framerate: config.framerate.max(1),
			need_input: 0,
			frame_index: 0,
			_com: com,
		}))
	}

	/// Block on the event queue until the next event, returning its type.
	fn next_event(&self) -> Result<u32, Error> {
		let event = unsafe { self.events.GetEvent(MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS(0)) }
			.map_err(|e| mf_err("get event", e))?;
		unsafe { event.GetType() }.map_err(|e| mf_err("event type", e))
	}

	/// Pull one METransformHaveOutput's worth of encoded data, append it to `out`.
	fn process_output(&mut self, out: &mut Vec<Bytes>) -> Result<(), Error> {
		let mut buffers = [MFT_OUTPUT_DATA_BUFFER {
			dwStreamID: 0,
			pSample: ManuallyDrop::new(if self.provides_samples {
				None
			} else {
				Some(self.alloc_output_sample()?)
			}),
			dwStatus: 0,
			pEvents: ManuallyDrop::new(None),
		}];
		let mut status = 0u32;
		let result = unsafe { self.transform.ProcessOutput(0, &mut buffers, &mut status) };

		let sample = unsafe { ManuallyDrop::take(&mut buffers[0].pSample) };
		unsafe { ManuallyDrop::drop(&mut buffers[0].pEvents) };

		match result {
			Ok(()) => {}
			// The MFT changed its mind and has nothing for us; not an error.
			Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
			Err(e) => return Err(mf_err("process output", e)),
		}

		if let Some(sample) = sample {
			if let Some(packet) = self.sample_to_annexb(&sample)? {
				out.push(packet);
			}
		}
		Ok(())
	}

	/// Convert one output sample (Annex-B byte stream) to a packet, prepending
	/// the cached SPS/PPS on a keyframe if it isn't already in-band.
	fn sample_to_annexb(&self, sample: &IMFSample) -> Result<Option<Bytes>, Error> {
		let buffer = unsafe {
			sample
				.ConvertToContiguousBuffer()
				.map_err(|e| mf_err("contiguous buffer", e))?
		};
		let mut ptr_out: *mut u8 = ptr::null_mut();
		let mut len: u32 = 0;
		unsafe {
			buffer
				.Lock(&mut ptr_out, None, Some(&mut len))
				.map_err(|e| mf_err("lock output", e))?;
		}
		let data = unsafe { slice::from_raw_parts(ptr_out, len as usize) }.to_vec();
		unsafe {
			let _ = buffer.Unlock();
		}
		if data.is_empty() {
			return Ok(None);
		}

		let keyframe = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) != 0;
		// Prepend SPS/PPS only when this is a keyframe and the encoder didn't
		// already emit them in-band. Some encoders lead the IDR with an access
		// unit delimiter, so scan the whole AU for an SPS (NAL type 7), not just
		// the first NAL.
		if keyframe && !self.sequence_header.is_empty() && !contains_nal_type(&data, 7) {
			let mut out = BytesMut::with_capacity(self.sequence_header.len() + data.len());
			out.put_slice(&self.sequence_header);
			out.put_slice(&data);
			return Ok(Some(out.freeze()));
		}
		Ok(Some(Bytes::from(data)))
	}

	fn alloc_output_sample(&self) -> Result<IMFSample, Error> {
		let info = unsafe { self.transform.GetOutputStreamInfo(0) }.map_err(|e| mf_err("output stream info", e))?;
		let size = info.cbSize.max(1);
		let buffer = unsafe { MFCreateMemoryBuffer(size) }.map_err(|e| mf_err("output buffer", e))?;
		let sample = unsafe { MFCreateSample() }.map_err(|e| mf_err("output sample", e))?;
		unsafe { sample.AddBuffer(&buffer) }.map_err(|e| mf_err("add output buffer", e))?;
		Ok(sample)
	}

	/// Wrap a tightly-packed NV12 frame in an `IMFSample` with a monotonic
	/// presentation time (the encoder only needs strictly increasing stamps; the
	/// moq timestamp is attached downstream).
	fn nv12_sample(&mut self, nv12: &[u8]) -> Result<IMFSample, Error> {
		let buffer = unsafe { MFCreateMemoryBuffer(nv12.len() as u32) }.map_err(|e| mf_err("input buffer", e))?;
		let mut dst: *mut u8 = ptr::null_mut();
		unsafe {
			buffer.Lock(&mut dst, None, None).map_err(|e| mf_err("lock input", e))?;
			ptr::copy_nonoverlapping(nv12.as_ptr(), dst, nv12.len());
			let _ = buffer.Unlock();
			buffer
				.SetCurrentLength(nv12.len() as u32)
				.map_err(|e| mf_err("set input length", e))?;
		}

		let sample = unsafe { MFCreateSample() }.map_err(|e| mf_err("input sample", e))?;
		let pts = self.frame_index * HNS_PER_SEC / self.framerate as i64;
		let duration = HNS_PER_SEC / self.framerate as i64;
		self.frame_index += 1;
		unsafe {
			sample.AddBuffer(&buffer).map_err(|e| mf_err("add input buffer", e))?;
			sample.SetSampleTime(pts).map_err(|e| mf_err("set sample time", e))?;
			sample
				.SetSampleDuration(duration)
				.map_err(|e| mf_err("set sample duration", e))?;
		}
		Ok(sample)
	}
}

impl Backend for MediaFoundation {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		// The MFT input is NV12; capture hands us I420, so re-interleave the chroma.
		let i420 = frame.to_i420()?;
		let nv12 = i420_to_nv12(&i420);

		if keyframe {
			if let Some(api) = &self.codec_api {
				let _ = unsafe { api.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &VARIANT::from(1u32)) };
			}
		}

		let sample = self.nv12_sample(&nv12)?;
		let mut out = Vec::new();

		// Feed this frame as soon as the MFT asks for input, draining any output
		// buffered from earlier frames while we wait.
		loop {
			if self.need_input > 0 {
				unsafe { self.transform.ProcessInput(0, &sample, 0) }.map_err(|e| mf_err("process input", e))?;
				self.need_input -= 1;
				break;
			}
			match self.next_event()? {
				e if e == METransformNeedInput.0 as u32 => self.need_input += 1,
				e if e == METransformHaveOutput.0 as u32 => self.process_output(&mut out)?,
				_ => {}
			}
		}

		// Collect this frame's output if the encoder produces it promptly (low
		// latency). If it asks for more input first (pipeline depth > 0), defer:
		// the output surfaces on a later call rather than deadlocking here.
		loop {
			match self.next_event()? {
				e if e == METransformHaveOutput.0 as u32 => {
					self.process_output(&mut out)?;
					break;
				}
				e if e == METransformNeedInput.0 as u32 => {
					self.need_input += 1;
					break;
				}
				_ => {}
			}
		}

		Ok(out)
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		let mut out = Vec::new();
		unsafe {
			self.transform
				.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0)
				.map_err(|e| mf_err("end of stream", e))?;
			self.transform
				.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0)
				.map_err(|e| mf_err("drain", e))?;
		}

		// Pump until the MFT reports the drain is complete.
		loop {
			match self.next_event()? {
				e if e == METransformHaveOutput.0 as u32 => self.process_output(&mut out)?,
				e if e == METransformDrainComplete.0 as u32 => break,
				e if e == METransformNeedInput.0 as u32 => self.need_input += 1,
				_ => {}
			}
		}

		unsafe {
			let _ = self.transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
		}
		Ok(out)
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// Enumerate hardware H.264 encoder MFTs (NV12 in, H.264 out) and activate the
/// first one. Errors if the machine has no hardware encoder.
fn enumerate_hardware() -> Result<IMFTransform, Error> {
	let input = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: MFVideoFormat_NV12,
	};
	let output = MFT_REGISTER_TYPE_INFO {
		guidMajorType: MFMediaType_Video,
		guidSubtype: MFVideoFormat_H264,
	};

	let mut activates: *mut Option<IMFActivate> = ptr::null_mut();
	let mut count: u32 = 0;
	unsafe {
		MFTEnumEx(
			MFT_CATEGORY_VIDEO_ENCODER,
			MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
			Some(&input),
			Some(&output),
			&mut activates,
			&mut count,
		)
		.map_err(|e| mf_err("enumerate encoders", e))?;
	}
	if count == 0 {
		return Err(Error::Codec(anyhow::anyhow!("no hardware H.264 encoder found")));
	}

	let entries = unsafe { slice::from_raw_parts_mut(activates, count as usize) };
	let mut transform: Option<IMFTransform> = None;
	let mut activate_err = None;
	for slot in entries.iter_mut() {
		let Some(activate) = slot.take() else { continue };
		if transform.is_none() {
			match unsafe { activate.ActivateObject::<IMFTransform>() } {
				Ok(t) => transform = Some(t),
				Err(e) => activate_err = Some(e),
			}
		}
	}
	unsafe {
		windows::Win32::System::Com::CoTaskMemFree(Some(activates as *const _));
	}

	transform.ok_or_else(|| match activate_err {
		Some(e) => mf_err("activate encoder", e),
		None => Error::Codec(anyhow::anyhow!("hardware H.264 encoder failed to activate")),
	})
}

fn set_output_type(transform: &IMFTransform, config: &Config) -> Result<(), Error> {
	let ty = unsafe { MFCreateMediaType() }.map_err(|e| mf_err("create output type", e))?;
	unsafe {
		ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
			.map_err(|e| mf_err("output major type", e))?;
		ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
			.map_err(|e| mf_err("output subtype", e))?;
		ty.SetUINT32(&MF_MT_AVG_BITRATE, clamp_u32(config.resolved_bitrate()))
			.map_err(|e| mf_err("output bitrate", e))?;
		ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
			.map_err(|e| mf_err("output interlace", e))?;
		ty.SetUINT64(&MF_MT_FRAME_SIZE, pack_2x32(config.width, config.height))
			.map_err(|e| mf_err("output frame size", e))?;
		ty.SetUINT64(&MF_MT_FRAME_RATE, pack_2x32(config.framerate.max(1), 1))
			.map_err(|e| mf_err("output frame rate", e))?;
		ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_2x32(1, 1))
			.map_err(|e| mf_err("output aspect", e))?;
		ty.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
			.map_err(|e| mf_err("output profile", e))?;
		transform
			.SetOutputType(0, &ty, 0)
			.map_err(|e| mf_err("set output type", e))?;
	}
	Ok(())
}

fn set_input_type(transform: &IMFTransform, config: &Config) -> Result<(), Error> {
	let ty = unsafe { MFCreateMediaType() }.map_err(|e| mf_err("create input type", e))?;
	unsafe {
		ty.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
			.map_err(|e| mf_err("input major type", e))?;
		ty.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)
			.map_err(|e| mf_err("input subtype", e))?;
		ty.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
			.map_err(|e| mf_err("input interlace", e))?;
		ty.SetUINT64(&MF_MT_FRAME_SIZE, pack_2x32(config.width, config.height))
			.map_err(|e| mf_err("input frame size", e))?;
		ty.SetUINT64(&MF_MT_FRAME_RATE, pack_2x32(config.framerate.max(1), 1))
			.map_err(|e| mf_err("input frame rate", e))?;
		ty.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack_2x32(1, 1))
			.map_err(|e| mf_err("input aspect", e))?;
		transform
			.SetInputType(0, &ty, 0)
			.map_err(|e| mf_err("set input type", e))?;
	}
	Ok(())
}

/// Read the Annex-B SPS/PPS blob from the negotiated output type, if present.
/// Empty if the encoder doesn't expose one (then we rely on in-band SPS/PPS).
fn read_sequence_header(transform: &IMFTransform) -> Vec<u8> {
	let Ok(ty) = (unsafe { transform.GetOutputCurrentType(0) }) else {
		return Vec::new();
	};
	// Size the blob, then fetch it.
	let len = match unsafe { ty.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER) } {
		Ok(n) if n > 0 => n,
		_ => return Vec::new(),
	};
	let mut blob = vec![0u8; len as usize];
	let mut written: u32 = 0;
	if unsafe { ty.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut blob, Some(&mut written)) }.is_err() {
		return Vec::new();
	}
	blob.truncate(written as usize);
	blob
}

/// Re-interleave planar I420 chroma into NV12 (Y plane unchanged, U/V packed as
/// alternating bytes). The inverse of the capture's `I420::from_nv12`.
fn i420_to_nv12(i420: &crate::frame::I420) -> Vec<u8> {
	let (y, u, v) = (i420.y(), i420.u(), i420.v());
	let mut out = Vec::with_capacity(y.len() + u.len() + v.len());
	out.extend_from_slice(y);
	for (cu, cv) in u.iter().zip(v.iter()) {
		out.push(*cu);
		out.push(*cv);
	}
	out
}

/// Whether an Annex-B buffer contains a NAL of the given type. Scans for 3-byte
/// start codes, which also matches the tail of any 4-byte `00 00 00 01` code.
fn contains_nal_type(data: &[u8], nal_type: u8) -> bool {
	let mut i = 0;
	while i + 3 < data.len() {
		if data[i..i + 3] == [0, 0, 1] {
			if data[i + 3] & 0x1f == nal_type {
				return true;
			}
			i += 3;
		} else {
			i += 1;
		}
	}
	false
}

fn clamp_u32(value: u64) -> u32 {
	value.min(u32::MAX as u64) as u32
}
