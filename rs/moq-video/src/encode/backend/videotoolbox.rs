//! Hardware H.264 / H.265 backend via Apple VideoToolbox (`VTCompressionSession`).
//!
//! VideoToolbox emits AVCC/HVCC (length-prefixed NALs) with parameter sets
//! (SPS/PPS, plus VPS for H.265) carried out-of-band in the sample's format
//! description. We convert to Annex-B in-band so the output matches every other
//! backend (`moq_mux` avc3 / hev1 mode): the encoded slice lengths become start
//! codes, and on each keyframe we prepend the parameter sets pulled from the
//! format description.
//!
//! Rate control holds every second, and every few frames, to the bitrate with
//! `DataRateLimits`, the only limit VideoToolbox honors in real time; it keeps
//! the limit by aiming its average about 30% below it. The one exception seen is
//! a keyframe landing on a cut from a still scene, whose second H.265 ran up to
//! 12% over. A quantizer floor
//! (`MinAllowedFrameQP`) is the quality: a calm scene the floor already codes
//! cleanly spends less, and a busy one gets a coarser quantizer up to the limit.
//! Measured on an M4, the alternatives miss the cap: `Quality` is constant
//! quality but ignores the limit and the average once set, `ConstantBitRate` and
//! `VariableBitRate` with `VBVMaxBitRate` let a second run 15% over, and the
//! low-latency rate control ignores `DataRateLimits`.
//!
//! Hand-written on the raw `objc2-video-toolbox` bindings; there's no
//! higher-level crate we trust. The backend is `!Send` and a direct `Encoder` is
//! thread-bound with it; only the Apple `Sink::Inner` keeps the serialized
//! `Send` wrapper, safe because `Sink` serializes every call.

use std::collections::VecDeque;
use std::ffi::{c_int, c_void};
use std::ptr::{self, NonNull};
use std::slice;
use std::time::Duration;

use bytes::{BufMut, Bytes, BytesMut};
use moq_net::Timestamp;
use objc2_core_foundation::{
	CFArray, CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType, kCFBooleanFalse, kCFBooleanTrue,
};
use objc2_core_media::{
	CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetH264ParameterSetAtIndex,
	CMVideoFormatDescriptionGetHEVCParameterSetAtIndex, kCMTimeInvalid, kCMVideoCodecType_H264, kCMVideoCodecType_HEVC,
};
use objc2_core_video::CVImageBuffer;
use objc2_core_video::{
	kCVImageBufferColorPrimaries_EBU_3213, kCVImageBufferColorPrimaries_ITU_R_709_2,
	kCVImageBufferColorPrimaries_ITU_R_2020, kCVImageBufferColorPrimaries_SMPTE_C,
	kCVImageBufferTransferFunction_ITU_R_709_2, kCVImageBufferTransferFunction_ITU_R_2100_HLG,
	kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ, kCVImageBufferYCbCrMatrix_ITU_R_601_4,
	kCVImageBufferYCbCrMatrix_ITU_R_709_2, kCVImageBufferYCbCrMatrix_ITU_R_2020,
};
use objc2_video_toolbox::{
	VTCompressionSession, VTEncodeInfoFlags, VTSessionCopySupportedPropertyDictionary, VTSessionSetProperty,
	kVTCompressionPropertyKey_AllowFrameReordering, kVTCompressionPropertyKey_AverageBitRate,
	kVTCompressionPropertyKey_ColorPrimaries, kVTCompressionPropertyKey_DataRateLimits,
	kVTCompressionPropertyKey_ExpectedFrameRate, kVTCompressionPropertyKey_MaxKeyFrameInterval,
	kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality, kVTCompressionPropertyKey_ProfileLevel,
	kVTCompressionPropertyKey_RealTime, kVTCompressionPropertyKey_TransferFunction,
	kVTCompressionPropertyKey_YCbCrMatrix, kVTEncodeFrameOptionKey_ForceKeyFrame, kVTProfileLevel_H264_High_AutoLevel,
	kVTProfileLevel_HEVC_Main_AutoLevel,
};

use super::super::encoder::{Applied, Codec, Config, Preset};
use super::{Backend, Encoded, keyframe_nal};
use crate::color::{Matrix, Primaries, Transfer};
use crate::frame::Surface;
use crate::{Error, Frame};

pub(crate) const NAME: &str = "videotoolbox";

/// `kVTCompressionPropertyKey_MinAllowedFrameQP`, spelled out because the
/// symbol is macOS 13 and iOS 16: linking it would stop the library loading on
/// macOS 12.
const MIN_QP: &str = "MinAllowedFrameQP";

/// Frames in the limit's short window. With the one-second window alone, a
/// keyframe landing on a cut from a still scene took most of the second and
/// H.265 then dropped 20 frames to pay it back; five frames kept every frame and
/// cost about 1% of the busy rate. VideoToolbox lets a keyframe past this window,
/// so it damps the burst rather than bounding it.
const BURST_FRAMES: u32 = 5;

/// The quantizer floor for each codec: the coarsest that kept a 720p30 talking
/// head at VMAF 95, the usual mark for transparent, on an M4. H.264 under a 4
/// Mbit/s cap: 95.5 at 1.74 Mbit/s against 96.1 at 4.0 with no floor or limit,
/// and 26 fell to 94.8. H.265 under 2.8 Mbit/s: 95.2 at 1.52 against 96.4 at
/// 2.8, and 24 fell to 94.9.
fn min_qp(codec: Codec) -> i32 {
	match codec {
		Codec::H264 => 25,
		Codec::H265 => 23,
	}
}

/// Where the C output callback queues finished frames, popped by `receive`.
/// Lives behind a `Box` so its address is stable for the lifetime of the
/// session that holds it as a refcon.
struct Sink {
	codec: Codec,
	/// The timestamp of the frame going in, which every access unit the callback
	/// sees during its `complete_frames` came from.
	timestamp: Timestamp,
	ready: VecDeque<Encoded>,
	error: Option<i32>,
}

pub(crate) struct VideoToolbox {
	session: CFRetained<VTCompressionSession>,
	sink: Box<Sink>,
	/// `{ ForceKeyFrame: true }`, built once and reused for forced IDRs.
	force_keyframe: CFRetained<CFDictionary>,
	framerate_numerator: i32,
	framerate_denominator: i64,
	/// The limit's short window, [`BURST_FRAMES`] at the frame rate.
	burst: Duration,
	frame_index: i64,
	/// A cut whose frame VideoToolbox dropped, carried to the next frame so the
	/// group it asked for still opens.
	keyframe_pending: bool,
	applied: Applied,
}

impl VideoToolbox {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(config)?))
	}

	fn new(config: &Config) -> Result<Self, Error> {
		// backend::open only routes codecs this backend advertises, so the match is
		// exhaustive; a new Codec variant won't compile here until it's handled.
		let codec_type = match config.codec {
			Codec::H264 => kCMVideoCodecType_H264,
			Codec::H265 => kCMVideoCodecType_HEVC,
		};

		let mut sink = Box::new(Sink {
			codec: config.codec,
			timestamp: Timestamp::ZERO,
			ready: VecDeque::new(),
			error: None,
		});
		let refcon = (&mut *sink as *mut Sink).cast::<c_void>();

		let mut preset = config.preset;
		let (session, controls) = loop {
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
				.ok_or_else(|| Error::codec(anyhow::anyhow!("VTCompressionSessionCreate failed: {status}")))?;

			// Every preset forbids reordering. A failure here cannot be fixed by downgrading.
			set_bool(
				&session,
				unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
				false,
			)?;
			match apply_preset(&session, preset) {
				Ok(controls) => break (session, controls),
				Err(error) => {
					preset = match preset {
						Preset::Quality => Preset::Balanced,
						Preset::Balanced => Preset::LowLatency,
						Preset::LowLatency => return Err(error),
					};
					tracing::debug!(?error, ?preset, "VideoToolbox preset refused; trying lower preset");
					// Discard partial settings before trying the next preset.
				}
			}
		};
		let profile = unsafe {
			match config.codec {
				Codec::H265 => kVTProfileLevel_HEVC_Main_AutoLevel,
				_ => kVTProfileLevel_H264_High_AutoLevel,
			}
		};
		set_property(&session, unsafe { kVTCompressionPropertyKey_ProfileLevel }, profile)?;
		let burst = Duration::from_secs_f64(
			f64::from(BURST_FRAMES) * f64::from(config.framerate.denominator())
				/ f64::from(config.framerate.numerator()),
		);
		set_rate(&session, config.resolved_bitrate().as_bps(), burst)?;
		// The floor is the quality; without one (before macOS 13 and iOS 16) the
		// limit alone holds the rate, still never past the bitrate.
		let rate = match set_floor(&session, config.codec)? {
			Some(qp) => format!("QP floor {qp} under a {BURST_FRAMES}-frame and 1 s limit at the bitrate"),
			None => format!("{BURST_FRAMES}-frame and 1 s limit at the bitrate"),
		};
		set_number(
			&session,
			unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval },
			clamp_i32(config.keyframe_interval().into()),
		)?;
		set_number(
			&session,
			unsafe { kVTCompressionPropertyKey_ExpectedFrameRate },
			config.framerate.rounded() as i32,
		)?;

		// State the color space in the SPS so a decoder doesn't fall back to
		// guessing it from the frame height. The SDR curve goes out as BT.709 (1)
		// whatever the matrix: CoreVideo's 170M transfer constant is deprecated
		// and Media Foundation has none, so 1 is what every backend emits.
		let color = config.resolved_color();
		let (primaries, transfer, matrix) = unsafe {
			(
				match color.primaries {
					Primaries::Smpte170m => kCVImageBufferColorPrimaries_SMPTE_C,
					Primaries::Bt470bg => kCVImageBufferColorPrimaries_EBU_3213,
					Primaries::Bt709 => kCVImageBufferColorPrimaries_ITU_R_709_2,
					Primaries::Bt2020 => kCVImageBufferColorPrimaries_ITU_R_2020,
				},
				match color.transfer {
					Transfer::Bt709 => kCVImageBufferTransferFunction_ITU_R_709_2,
					Transfer::Pq => kCVImageBufferTransferFunction_SMPTE_ST_2084_PQ,
					Transfer::Hlg => kCVImageBufferTransferFunction_ITU_R_2100_HLG,
				},
				match color.matrix {
					Matrix::Bt601 => kCVImageBufferYCbCrMatrix_ITU_R_601_4,
					Matrix::Bt709 => kCVImageBufferYCbCrMatrix_ITU_R_709_2,
					Matrix::Bt2020 => kCVImageBufferYCbCrMatrix_ITU_R_2020,
				},
			)
		};
		set_property(&session, unsafe { kVTCompressionPropertyKey_ColorPrimaries }, primaries)?;
		set_property(
			&session,
			unsafe { kVTCompressionPropertyKey_TransferFunction },
			transfer,
		)?;
		set_property(&session, unsafe { kVTCompressionPropertyKey_YCbCrMatrix }, matrix)?;

		let force_keyframe = force_keyframe_dict()?;

		tracing::info!(
			encoder = NAME,
			codec = ?config.codec,
			width = config.width,
			height = config.height,
			"opened video encoder"
		);
		let framerate_numerator = i32::try_from(config.framerate.numerator())
			.map_err(|_| Error::codec(anyhow::anyhow!("VideoToolbox frame-rate numerator exceeds i32")))?;
		Ok(Self {
			session,
			sink,
			force_keyframe,
			framerate_numerator,
			framerate_denominator: i64::from(config.framerate.denominator()),
			burst,
			frame_index: 0,
			keyframe_pending: false,
			applied: Applied::new(preset, format!("{controls}, {rate}")),
		})
	}
}

impl Backend for VideoToolbox {
	fn encode(&mut self, frame: &Frame, cut: bool) -> Result<(), Error> {
		self.sink.error = None;
		// `complete_frames` below forces this frame out before returning, so every
		// access unit the callback sees until then came from it.
		self.sink.timestamp = frame.timestamp;
		let before = self.sink.ready.len();

		// Zero-copy when the capture handed us a surface; otherwise upload I420.
		let pixel_buffer = match &frame.surface {
			Surface::PixelBuffer(surface) => surface.buffer.clone(),
			Surface::I420(i420) => crate::frame::apple::upload_i420(i420)?,
		};
		let image: &CVImageBuffer = &pixel_buffer;

		// Presentation timestamps must strictly increase; the moq timestamp is
		// attached downstream, so a monotonic frame index over the framerate is
		// all VideoToolbox needs.
		let pts = unsafe {
			CMTime::new(
				self.frame_index.saturating_mul(self.framerate_denominator),
				self.framerate_numerator,
			)
		};
		self.frame_index += 1;

		let cut = cut || self.keyframe_pending;
		let frame_properties = cut.then_some(&*self.force_keyframe);

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
			return Err(Error::codec(anyhow::anyhow!(
				"VTCompressionSessionEncodeFrame failed: {status}"
			)));
		}

		// Force this frame out; with no reordering there's nothing else pending.
		let status = unsafe { self.session.complete_frames(kCMTimeInvalid) };
		if status != 0 {
			return Err(Error::codec(anyhow::anyhow!(
				"VTCompressionSessionCompleteFrames failed: {status}"
			)));
		}

		if let Some(status) = self.sink.error.take() {
			return Err(Error::codec(anyhow::anyhow!(
				"VideoToolbox encode callback failed: {status}"
			)));
		}
		// A dropped frame comes back with no packet and no error.
		self.keyframe_pending = cut && self.sink.ready.len() == before;
		Ok(())
	}

	fn receive(&mut self) -> Result<Option<Encoded>, Error> {
		Ok(self.sink.ready.pop_front())
	}

	fn flush(&mut self) -> Result<(), Error> {
		// complete_frames runs per-encode, so nothing is ever buffered.
		Ok(())
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		// Both rate keys are settable on a live session and take effect without
		// an IDR, which is exactly what the rate control loop wants.
		set_rate(&self.session, bitrate, self.burst)
	}

	fn can_cut(&self) -> bool {
		true
	}

	fn name(&self) -> &'static str {
		NAME
	}

	fn applied(&self) -> Applied {
		self.applied.clone()
	}
}

/// Hold every second of output, and every `burst`, to `bitrate`, the average
/// aimed at too, as Apple asks alongside a limit (VideoToolbox still aims about
/// 30% below the limit).
fn set_rate(session: &VTCompressionSession, bitrate: u64, burst: Duration) -> Result<(), Error> {
	set_number(
		session,
		unsafe { kVTCompressionPropertyKey_AverageBitRate },
		clamp_i32(bitrate),
	)?;
	let bytes = |window: Duration| CFNumber::new_f64(bitrate as f64 / 8.0 * window.as_secs_f64());
	let limits = [burst, Duration::from_secs(1)].map(|window| [bytes(window), CFNumber::new_f64(window.as_secs_f64())]);
	let limits: Vec<&CFNumber> = limits.iter().flatten().map(|number| &**number).collect();
	set_property(
		session,
		unsafe { kVTCompressionPropertyKey_DataRateLimits },
		&CFArray::from_objects(&limits),
	)
}

/// Set the codec's quantizer floor when the session offers one, returning it.
fn set_floor(session: &VTCompressionSession, codec: Codec) -> Result<Option<i32>, Error> {
	let key = CFString::from_static_str(MIN_QP);
	if !supports(session, &key)? {
		return Ok(None);
	}
	let qp = min_qp(codec);
	set_number(session, &key, qp)?;
	Ok(Some(qp))
}

/// Whether the session lists `key` among the properties it supports.
fn supports(session: &VTCompressionSession, key: &CFString) -> Result<bool, Error> {
	let mut supported: *const CFDictionary = ptr::null();
	let status = unsafe { VTSessionCopySupportedPropertyDictionary(session, NonNull::from(&mut supported)) };
	let supported = NonNull::new(supported.cast_mut())
		.filter(|_| status == 0)
		.map(|p| unsafe { CFRetained::from_raw(p) })
		.ok_or_else(|| {
			Error::codec(anyhow::anyhow!(
				"VTSessionCopySupportedPropertyDictionary failed: {status}"
			))
		})?;
	Ok(unsafe { supported.contains_ptr_key((key as *const CFString).cast()) })
}

fn apply_preset(session: &VTCompressionSession, preset: Preset) -> Result<&'static str, Error> {
	set_bool(
		session,
		unsafe { kVTCompressionPropertyKey_RealTime },
		preset == Preset::LowLatency,
	)?;
	let controls = match preset {
		Preset::LowLatency => "real-time, no frame reordering",
		Preset::Balanced | Preset::Quality => {
			set_bool(
				session,
				unsafe { kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality },
				preset == Preset::Balanced,
			)?;
			match preset {
				Preset::Balanced => "non-real-time, speed priority, no frame reordering",
				_ => "non-real-time, quality priority, no frame reordering",
			}
		}
	};
	Ok(controls)
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
		Ok(Some((payload, keyframe))) => sink.ready.push_back(Encoded::new(payload, sink.timestamp, keyframe)),
		Ok(None) => {}
		Err(status) => sink.error = Some(status),
	}
}

/// Convert one AVCC/HVCC `CMSampleBuffer` into a single Annex-B access unit. On a
/// keyframe, prepend the parameter sets (SPS/PPS for H.264; VPS/SPS/PPS for
/// H.265) from the format description so the stream is self-contained (avc3 / hev1).
/// Returns the access unit and whether it is a keyframe.
fn annexb_from_sample(sample: &CMSampleBuffer, codec: Codec) -> Result<Option<(Bytes, bool)>, i32> {
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
	if total == 0 {
		return Ok(None);
	}

	// `data_pointer` only guarantees `length_at_offset` contiguous bytes at
	// `data_ptr`; a non-contiguous block buffer would make a `total`-length slice
	// read past the mapped region. VideoToolbox output is contiguous in practice,
	// but copy the whole access unit flat if it ever isn't.
	let owned;
	let avcc: &[u8] = if !data_ptr.is_null() && length_at_offset >= total {
		unsafe { slice::from_raw_parts(data_ptr as *const u8, total) }
	} else {
		let mut buf = vec![0u8; total];
		let dst = NonNull::new(buf.as_mut_ptr().cast::<c_void>()).ok_or(-1)?;
		let status = unsafe { block.copy_data_bytes(0, total, dst) };
		if status != 0 {
			return Err(status);
		}
		owned = buf;
		&owned
	};

	let slices = split_avcc(avcc, nal_length_size as usize);
	let is_keyframe = slices.iter().any(|nal| keyframe_nal(codec, nal));

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

	Ok(Some((out.freeze(), is_keyframe)))
}

/// Dispatch to the codec-specific VideoToolbox parameter-set getter. Both have
/// identical signatures; only the codec differs.
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
	.ok_or_else(|| Error::codec(anyhow::anyhow!("failed to build force-keyframe dictionary")))
}

fn set_property(session: &VTCompressionSession, key: &CFString, value: &CFType) -> Result<(), Error> {
	let status = unsafe { VTSessionSetProperty(session, key, Some(value)) };
	if status != 0 {
		return Err(Error::codec(anyhow::anyhow!("VTSessionSetProperty failed: {status}")));
	}
	Ok(())
}

fn set_bool(session: &VTCompressionSession, key: &CFString, value: bool) -> Result<(), Error> {
	let boolean = unsafe { if value { kCFBooleanTrue } else { kCFBooleanFalse } }.unwrap();
	set_property(session, key, boolean.as_ref())
}

fn set_number(session: &VTCompressionSession, key: &CFString, value: i32) -> Result<(), Error> {
	let number = unsafe { CFNumber::new(None, CFNumberType::SInt32Type, &value as *const i32 as *const c_void) }
		.ok_or_else(|| Error::codec(anyhow::anyhow!("failed to build CFNumber")))?;
	set_property(session, key, number.as_ref())
}

fn clamp_i32(value: u64) -> i32 {
	value.min(i32::MAX as u64) as i32
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::frame::I420;
	use objc2_video_toolbox::VTSessionCopyProperty;

	struct Probe(CFRetained<VTCompressionSession>);

	impl Drop for Probe {
		fn drop(&mut self) {
			unsafe { self.0.invalidate() };
		}
	}

	fn read_bool(session: &VTCompressionSession, key: &CFString) -> bool {
		let mut value: *mut CFType = ptr::null_mut();
		let status = unsafe { VTSessionCopyProperty(session, key, None, (&mut value as *mut *mut CFType).cast()) };
		assert_eq!(status, 0);
		let value = unsafe { CFRetained::from_raw(NonNull::new(value).unwrap()) };
		let yes = unsafe { kCFBooleanTrue }.unwrap();
		let no = unsafe { kCFBooleanFalse }.unwrap();
		assert!(&*value == yes.as_ref() || &*value == no.as_ref());
		&*value == yes.as_ref()
	}

	#[test]
	fn min_qp_names_the_sdk_key() {
		let key = unsafe { objc2_video_toolbox::kVTCompressionPropertyKey_MinAllowedFrameQP };
		assert_eq!(&*CFString::from_static_str(MIN_QP), key);
	}

	/// A calm scene spends well under a generous cap, held there by the
	/// quantizer floor; a busy one is held under its cap, and under a lower one
	/// once `set_bitrate` moves the limit. One opening keyframe, so the seconds
	/// measured are rate control's alone.
	#[test]
	fn quality_under_a_cap() {
		use super::super::test_util::spend;

		for codec in [Codec::H264, Codec::H265] {
			let mut config = Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap());
			config.kind = crate::encode::Kind::Named(NAME.into());
			config.codec = codec;
			config.join = std::time::Duration::from_secs(10);
			let at = |bps: u64| Config {
				bitrate: Some(moq_net::bandwidth::Rate::from_bps(bps)),
				..config.clone()
			};

			let calm = spend(&at(4_000_000), false, None).unwrap();
			// An M4 spent 35-50 kbit/s here, and 340 with no floor or limit.
			assert!(calm.mean < 150_000, "{codec:?}: a calm scene filled the cap: {calm:?}");

			let busy = spend(&at(4_000_000), true, None).unwrap();
			// VideoToolbox aims about 30% under its limit, and an M4 coded noise at
			// 53-57% of this one.
			assert!(
				busy.mean > 1_500_000,
				"{codec:?}: a busy scene fell short of the cap: {busy:?}"
			);
			assert!(
				busy.peak <= 4_000_000,
				"{codec:?}: a busy scene broke the cap: {busy:?}"
			);

			// Under a cap this tight noise is incompressible: VideoToolbox drops
			// frames rather than break the limit (an average alone ran to 720
			// kbit/s), so only the peak is checked.
			let moved = spend(&at(4_000_000), true, Some(250_000)).unwrap();
			assert!(
				moved.peak <= 250_000,
				"{codec:?}: the limit did not follow set_bitrate: {moved:?}"
			);
		}
	}

	#[test]
	fn presets_reach_the_session_without_buffering_frames() {
		for codec in [Codec::H264, Codec::H265] {
			for preset in [Preset::LowLatency, Preset::Balanced, Preset::Quality] {
				let config = Config {
					codec,
					preset,
					..Config::new(320, 240, crate::FrameRate::new(30, 1).unwrap())
				};
				// Determine which controls this host accepts independently of the production fallback.
				let candidates: &[Preset] = match preset {
					Preset::Quality => &[Preset::Quality, Preset::Balanced, Preset::LowLatency],
					Preset::Balanced => &[Preset::Balanced, Preset::LowLatency],
					Preset::LowLatency => &[Preset::LowLatency],
				};
				let expected = candidates
					.iter()
					.copied()
					.find(|&candidate| {
						// A refused property may mutate the session, so each candidate starts fresh.
						let codec_type = match codec {
							Codec::H264 => kCMVideoCodecType_H264,
							Codec::H265 => kCMVideoCodecType_HEVC,
						};
						let mut session_ptr = ptr::null_mut();
						let status = unsafe {
							VTCompressionSession::create(
								None,
								config.width as i32,
								config.height as i32,
								codec_type,
								None,
								None,
								None,
								None,
								ptr::null_mut(),
								NonNull::new(&mut session_ptr).unwrap(),
							)
						};
						assert_eq!(status, 0, "{codec:?} {candidate:?} probe session creation failed");
						let probe = Probe(unsafe {
							CFRetained::from_raw(NonNull::new(session_ptr).expect("probe session is null"))
						});
						set_bool(
							&probe.0,
							unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
							false,
						)
						.expect("probe must support disabling frame reordering");
						let realtime = set_bool(
							&probe.0,
							unsafe { kVTCompressionPropertyKey_RealTime },
							candidate == Preset::LowLatency,
						);
						if candidate == Preset::LowLatency {
							realtime.expect("probe must support the lowest preset");
							true
						} else {
							realtime
								.and_then(|()| {
									set_bool(
										&probe.0,
										unsafe { kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality },
										candidate == Preset::Balanced,
									)
								})
								.is_ok()
						}
					})
					.expect("probe must accept at least one preset");
				let mut encoder = VideoToolbox::new(&config).unwrap();
				assert_eq!(encoder.applied().preset, Some(expected), "{codec:?} {preset:?}");
				let size = config.size();
				for index in 0..3 {
					let timestamp = moq_net::Timestamp::from_micros(index * 33_333).unwrap();
					let frame = Frame::new(
						Surface::I420(I420::new(size, vec![0x80; I420::len(size).unwrap()]).unwrap()),
						timestamp,
					);
					let packets = encoder.encode_all(&frame, index == 1).unwrap();
					assert_eq!(packets.len(), 1, "{codec:?} {preset:?}");
					assert_eq!(packets[0].timestamp, timestamp);
					if index <= 1 {
						assert!(packets[0].keyframe);
					}
				}
				assert!(!read_bool(&encoder.session, unsafe {
					kVTCompressionPropertyKey_AllowFrameReordering
				}));
				assert_eq!(
					read_bool(&encoder.session, unsafe { kVTCompressionPropertyKey_RealTime }),
					expected == Preset::LowLatency
				);
				if expected != Preset::LowLatency {
					assert_eq!(
						read_bool(&encoder.session, unsafe {
							kVTCompressionPropertyKey_PrioritizeEncodingSpeedOverQuality
						}),
						expected == Preset::Balanced
					);
				}
				assert!(encoder.flush_all().unwrap().is_empty());
			}
		}
	}
}
