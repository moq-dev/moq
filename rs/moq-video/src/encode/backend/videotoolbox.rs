//! Hardware H.264 / H.265 backend via Apple VideoToolbox (`VTCompressionSession`).
//!
//! VideoToolbox emits AVCC/HVCC (length-prefixed NALs) with parameter sets
//! (SPS/PPS, plus VPS for H.265) carried out-of-band in the sample's format
//! description. We convert to Annex-B in-band so the output matches every other
//! backend (`moq_mux` avc3 / hev1 mode): the encoded slice lengths become start
//! codes, and on each keyframe we prepend the parameter sets pulled from the
//! format description.
//!
//! Hand-written on the raw `objc2-video-toolbox` bindings; there's no
//! higher-level crate we trust. Used only from the single capture/encode thread,
//! so the `!Send` CoreFoundation handles are wrapped in a thread-confined type.

use std::ffi::{c_int, c_void};
use std::ptr::{self, NonNull};
use std::slice;

use bytes::{BufMut, Bytes, BytesMut};
use objc2_core_foundation::{
	CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType, kCFBooleanFalse, kCFBooleanTrue,
};
use objc2_core_media::{
	CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
	CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::{
	CVImageBuffer, CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane,
	CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags,
	CVPixelBufferUnlockBaseAddress, kCVPixelFormatType_420YpCbCr8Planar,
};
use objc2_video_toolbox::{
	VTCompressionSession, VTEncodeInfoFlags, VTSessionSetProperty, kVTCompressionPropertyKey_AllowFrameReordering,
	kVTCompressionPropertyKey_AverageBitRate, kVTCompressionPropertyKey_ExpectedFrameRate,
	kVTCompressionPropertyKey_MaxKeyFrameInterval, kVTCompressionPropertyKey_ProfileLevel,
	kVTCompressionPropertyKey_RealTime, kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
	kVTProfileLevel_HEVC_Main_AutoLevel,
};

use super::super::encoder::{Codec, Config};
use super::Backend;
use crate::Error;
use crate::frame::{Frame, I420};

pub(crate) const NAME: &str = "videotoolbox";

/// Where the C output callback drops finished frames, read back after each
/// `encode_frame` + `complete_frames`. Lives behind a `Box` so its address is
/// stable for the lifetime of the session that holds it as a refcon.
struct Sink {
	codec: Codec,
	packets: Vec<Bytes>,
	error: Option<i32>,
}

pub(crate) struct VideoToolbox {
	session: CFRetained<VTCompressionSession>,
	sink: Box<Sink>,
	/// `{ ForceKeyFrame: true }`, built once and reused for forced IDRs.
	force_keyframe: CFRetained<CFDictionary>,
	framerate: i32,
	frame_index: i64,
}

// The session and its CoreFoundation handles are only ever touched from the one
// capture/encode thread (see `publish_capture`'s `spawn_blocking`).
unsafe impl Send for VideoToolbox {}

impl VideoToolbox {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		// backend::open only routes codecs this backend advertises, so the match is
		// exhaustive; a new Codec variant won't compile here until it's handled.
		let codec_type = match config.codec {
			Codec::H264 => kCMVideoCodecType_H264,
			Codec::H265 => kCMVideoCodecType_HEVC,
		};

		let mut sink = Box::new(Sink {
			codec: config.codec,
			packets: Vec::new(),
			error: None,
		});
		let refcon = (&mut *sink as *mut Sink).cast::<c_void>();

		let mut session_ptr: *mut VTCompressionSession = ptr::null_mut();
		let status = unsafe {
			VTCompressionSession::create(
				None,
				config.width as i32,
				config.height as i32,
				codec_type,
				None,
				None,
				None,
				Some(output_callback),
				refcon,
				NonNull::new(&mut session_ptr).unwrap(),
			)
		};
		let session = NonNull::new(session_ptr)
			.filter(|_| status == 0)
			.map(|p| unsafe { CFRetained::from_raw(p) })
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("VTCompressionSessionCreate failed: {status}")))?;

		set_bool(&session, unsafe { kVTCompressionPropertyKey_RealTime }, true)?;
		// Low latency: no frame reordering / B-frames.
		set_bool(
			&session,
			unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
			false,
		)?;
		let profile = unsafe {
			match config.codec {
				Codec::H265 => kVTProfileLevel_HEVC_Main_AutoLevel,
				_ => kVTProfileLevel_H264_High_AutoLevel,
			}
		};
		set_property(&session, unsafe { kVTCompressionPropertyKey_ProfileLevel }, profile)?;
		set_number(
			&session,
			unsafe { kVTCompressionPropertyKey_AverageBitRate },
			clamp_i32(config.resolved_bitrate()),
		)?;
		set_number(
			&session,
			unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval },
			config.gop as i32,
		)?;
		set_number(
			&session,
			unsafe { kVTCompressionPropertyKey_ExpectedFrameRate },
			config.framerate as i32,
		)?;

		let force_keyframe = force_keyframe_dict()?;

		tracing::info!(
			encoder = NAME,
			codec = ?config.codec,
			width = config.width,
			height = config.height,
			"opened video encoder"
		);
		Ok(Box::new(Self {
			session,
			sink,
			force_keyframe,
			framerate: config.framerate as i32,
			frame_index: 0,
		}))
	}
}

impl Backend for VideoToolbox {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		self.sink.packets.clear();
		self.sink.error = None;

		// Zero-copy when the capture handed us a surface; otherwise upload I420.
		let pixel_buffer = match frame {
			Frame::Surface(surface) => surface.buffer.clone(),
			Frame::I420(i420) => make_pixel_buffer(i420)?,
		};
		let image: &CVImageBuffer = &pixel_buffer;

		// Presentation timestamps must strictly increase; the moq timestamp is
		// attached downstream, so a monotonic frame index over the framerate is
		// all VideoToolbox needs.
		let pts = unsafe { CMTime::new(self.frame_index, self.framerate.max(1)) };
		self.frame_index += 1;

		let frame_properties = keyframe.then_some(&*self.force_keyframe);

		let status = unsafe {
			self.session.encode_frame(
				image,
				pts,
				kCMTimeInvalid,
				frame_properties,
				ptr::null_mut(),
				ptr::null_mut(),
			)
		};
		if status != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"VTCompressionSessionEncodeFrame failed: {status}"
			)));
		}

		// Force this frame out; with no reordering there's nothing else pending.
		let status = unsafe { self.session.complete_frames(kCMTimeInvalid) };
		if status != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"VTCompressionSessionCompleteFrames failed: {status}"
			)));
		}

		if let Some(status) = self.sink.error.take() {
			return Err(Error::Codec(anyhow::anyhow!(
				"VideoToolbox encode callback failed: {status}"
			)));
		}
		Ok(std::mem::take(&mut self.sink.packets))
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		// complete_frames runs per-encode, so nothing is buffered at shutdown.
		Ok(Vec::new())
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// C callback VideoToolbox invokes (synchronously, from `complete_frames`) for
/// each finished frame. Converts the AVCC sample to Annex-B and appends it.
unsafe extern "C-unwind" fn output_callback(
	refcon: *mut c_void,
	_source_frame_refcon: *mut c_void,
	status: i32,
	_flags: VTEncodeInfoFlags,
	sample_buffer: *mut CMSampleBuffer,
) {
	let sink = unsafe { &mut *(refcon as *mut Sink) };
	if status != 0 {
		sink.error = Some(status);
		return;
	}
	let Some(sample) = (unsafe { sample_buffer.as_ref() }) else {
		return; // dropped frame
	};
	match annexb_from_sample(sample, sink.codec) {
		Ok(Some(packet)) => sink.packets.push(packet),
		Ok(None) => {}
		Err(status) => sink.error = Some(status),
	}
}

/// Convert one AVCC/HVCC `CMSampleBuffer` into a single Annex-B access unit. On a
/// keyframe, prepend the parameter sets (SPS/PPS for H.264; VPS/SPS/PPS for
/// H.265) from the format description so the stream is self-contained (avc3 / hev1).
fn annexb_from_sample(sample: &CMSampleBuffer, codec: Codec) -> Result<Option<Bytes>, i32> {
	let format = unsafe { sample.format_description() }.ok_or(-1)?;

	// One call with null pointers just reports the count and NAL length size.
	let mut count: usize = 0;
	let mut nal_length_size: c_int = 4;
	let status = unsafe {
		get_param_set(
			&format,
			0,
			ptr::null_mut(),
			ptr::null_mut(),
			&mut count,
			&mut nal_length_size,
			codec,
		)
	};
	if status != 0 {
		return Err(status);
	}

	let block = unsafe { sample.data_buffer() }.ok_or(-1)?;
	let mut total: usize = 0;
	let mut length_at_offset: usize = 0;
	let mut data_ptr: *mut i8 = ptr::null_mut();
	let status = unsafe { block.data_pointer(0, &mut length_at_offset, &mut total, &mut data_ptr) };
	if status != 0 {
		return Err(status);
	}
	if data_ptr.is_null() || total == 0 {
		return Ok(None);
	}
	let avcc = unsafe { slice::from_raw_parts(data_ptr as *const u8, total) };

	let slices = split_avcc(avcc, nal_length_size as usize);
	let is_keyframe = slices.iter().any(|nal| is_keyframe_nal(nal, codec));

	let mut out = BytesMut::with_capacity(total + 64);
	if is_keyframe {
		for i in 0..count {
			let mut ptr: *const u8 = ptr::null();
			let mut size: usize = 0;
			let status =
				unsafe { get_param_set(&format, i, &mut ptr, &mut size, ptr::null_mut(), ptr::null_mut(), codec) };
			if status != 0 {
				return Err(status);
			}
			if !ptr.is_null() && size > 0 {
				append_annexb(&mut out, unsafe { slice::from_raw_parts(ptr, size) });
			}
		}
	}
	for nal in slices {
		append_annexb(&mut out, nal);
	}

	Ok(Some(out.freeze()))
}

/// Dispatch to the codec-specific VideoToolbox parameter-set getter. Both have
/// identical signatures; only the codec differs.
#[allow(clippy::too_many_arguments)]
unsafe fn get_param_set(
	format: &CMFormatDescription,
	index: usize,
	ptr_out: *mut *const u8,
	size_out: *mut usize,
	count_out: *mut usize,
	nal_len_out: *mut c_int,
	codec: Codec,
) -> i32 {
	match codec {
		Codec::H265 => unsafe {
			CMVideoFormatDescriptionGetHEVCParameterSetAtIndex(format, index, ptr_out, size_out, count_out, nal_len_out)
		},
		_ => unsafe {
			CMVideoFormatDescriptionGetH264ParameterSetAtIndex(format, index, ptr_out, size_out, count_out, nal_len_out)
		},
	}
}

/// Whether a NAL is a keyframe slice: an H.264 IDR (type 5), or an H.265 IRAP
/// picture (BLA/IDR/CRA, types 16..=23).
fn is_keyframe_nal(nal: &[u8], codec: Codec) -> bool {
	let Some(&b) = nal.first() else {
		return false;
	};
	match codec {
		Codec::H265 => {
			let nal_type = (b >> 1) & 0x3f;
			(16..=23).contains(&nal_type)
		}
		_ => b & 0x1f == 5,
	}
}

fn append_annexb(out: &mut BytesMut, nal: &[u8]) {
	out.put_slice(&[0, 0, 0, 1]);
	out.put_slice(nal);
}

/// Split a length-prefixed AVCC buffer into its NAL unit slices.
fn split_avcc(mut data: &[u8], length_size: usize) -> Vec<&[u8]> {
	let mut out = Vec::new();
	while data.len() > length_size {
		let mut len = 0usize;
		for &b in &data[..length_size] {
			len = (len << 8) | b as usize;
		}
		data = &data[length_size..];
		if len > data.len() {
			break; // truncated; bail rather than read out of bounds
		}
		let (nal, rest) = data.split_at(len);
		out.push(nal);
		data = rest;
	}
	out
}

/// Allocate a planar I420 `CVPixelBuffer` and copy the frame into it (the CPU
/// fallback, when capture didn't hand us a surface).
fn make_pixel_buffer(frame: &I420) -> Result<CFRetained<CVPixelBuffer>, Error> {
	let (w, h) = (frame.width as usize, frame.height as usize);
	let (cw, ch) = (w / 2, h / 2);

	let mut ptr: *mut CVPixelBuffer = ptr::null_mut();
	let status = unsafe {
		CVPixelBufferCreate(
			None,
			w,
			h,
			kCVPixelFormatType_420YpCbCr8Planar,
			None,
			NonNull::new(&mut ptr).unwrap(),
		)
	};
	let buffer = NonNull::new(ptr)
		.filter(|_| status == 0)
		.map(|p| unsafe { CFRetained::from_raw(p) })
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("CVPixelBufferCreate failed: {status}")))?;

	let flags = CVPixelBufferLockFlags(0);
	let status = unsafe { CVPixelBufferLockBaseAddress(&buffer, flags) };
	if status != 0 {
		return Err(Error::Codec(anyhow::anyhow!(
			"CVPixelBufferLockBaseAddress failed: {status}"
		)));
	}

	copy_plane(&buffer, 0, frame.y(), w, h);
	copy_plane(&buffer, 1, frame.u(), cw, ch);
	copy_plane(&buffer, 2, frame.v(), cw, ch);

	unsafe { CVPixelBufferUnlockBaseAddress(&buffer, flags) };
	Ok(buffer)
}

/// Copy a tightly-packed source plane into a pixel-buffer plane, honoring its
/// (possibly padded) row stride.
fn copy_plane(buffer: &CVPixelBuffer, plane: usize, src: &[u8], row_bytes: usize, rows: usize) {
	let base = CVPixelBufferGetBaseAddressOfPlane(buffer, plane) as *mut u8;
	let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, plane);
	for y in 0..rows {
		unsafe {
			let dst = base.add(y * stride);
			ptr::copy_nonoverlapping(src[y * row_bytes..].as_ptr(), dst, row_bytes);
		}
	}
}

fn force_keyframe_dict() -> Result<CFRetained<CFDictionary>, Error> {
	let key = (unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame } as *const CFString).cast::<c_void>();
	let value = unsafe { kCFBooleanTrue }.unwrap() as *const _ as *const c_void;
	let mut keys: [*const c_void; 1] = [key];
	let mut values: [*const c_void; 1] = [value];
	unsafe {
		CFDictionary::new(
			None,
			keys.as_mut_ptr(),
			values.as_mut_ptr(),
			1,
			&objc2_core_foundation::kCFTypeDictionaryKeyCallBacks,
			&objc2_core_foundation::kCFTypeDictionaryValueCallBacks,
		)
	}
	.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to build force-keyframe dictionary")))
}

fn set_property(session: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<(), Error> {
	let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
	if status != 0 {
		return Err(Error::Codec(anyhow::anyhow!("VTSessionSetProperty failed: {status}")));
	}
	Ok(())
}

fn set_bool(session: &VTCompressionSession, key: &CFString, value: bool) -> Result<(), Error> {
	let boolean = unsafe { if value { kCFBooleanTrue } else { kCFBooleanFalse } }.unwrap();
	set_property(session, key, boolean.as_ref())
}

fn set_number(session: &VTCompressionSession, key: &CFString, value: i32) -> Result<(), Error> {
	let number = unsafe { CFNumber::new(None, CFNumberType::SInt32Type, &value as *const i32 as *const c_void) }
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to build CFNumber")))?;
	set_property(session, key, number.as_ref())
}

fn clamp_i32(value: u64) -> i32 {
	value.min(i32::MAX as u64) as i32
}
