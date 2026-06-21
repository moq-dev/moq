//! Hardware H.264 decode backend via Apple VideoToolbox (`VTDecompressionSession`).
//!
//! The inverse of the encode VideoToolbox backend. We receive Annex-B access
//! units (SPS/PPS inline ahead of each keyframe), so we:
//! - pull SPS/PPS out of the stream and build a `CMVideoFormatDescription`,
//!   (re)creating the decompression session whenever the parameter sets change;
//! - repackage the slice NALs as AVCC (4-byte length-prefixed) in a
//!   `CMSampleBuffer`, the form VideoToolbox decodes;
//! - request NV12 output and download it to packed I420 (reusing the same
//!   `CVPixelBuffer` download as the capture path).
//!
//! Hand-written on the raw `objc2-video-toolbox` bindings; there's no
//! higher-level crate we trust. Decoding is synchronous (no async flag), so the
//! output callback fires from within `decode_frame` before it returns, which is
//! what lets the `!Send` CoreFoundation handles stay thread-confined.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use bytes::Bytes;
use moq_mux::codec::annexb::NalIterator;
use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CFRetained, CFString};
use objc2_core_media::{
	CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionCreateFromH264ParameterSets,
	kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
	CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth, kCVPixelBufferPixelFormatTypeKey,
	kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
	VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
};

use super::{Backend, Codec};
use crate::Error;
use crate::frame::I420;

pub(crate) const NAME: &str = "videotoolbox";

const NAL_TYPE_SPS: u8 = 7;
const NAL_TYPE_PPS: u8 = 8;

/// Where the C output callback drops decoded frames, drained after each
/// `decode_frame`. Boxed so its address is a stable refcon for the session.
#[derive(Default)]
struct Sink {
	frames: Vec<I420>,
	error: Option<String>,
}

pub(crate) struct VideoToolbox {
	/// Built lazily once the first SPS+PPS arrive, rebuilt if they change.
	session: Option<CFRetained<VTDecompressionSession>>,
	/// Format description the current session + samples use (kept in lockstep
	/// with `session`).
	format: Option<CFRetained<CMFormatDescription>>,
	/// Latest SPS/PPS seen, persisted across access units (a delta frame carries
	/// neither). `built_from` records which pair the live session was built from,
	/// so a mid-stream parameter-set change triggers a rebuild.
	sps: Option<Bytes>,
	pps: Option<Bytes>,
	built_from: Option<(Bytes, Bytes)>,
	sink: Box<Sink>,
}

// The session and its CoreFoundation handles are only ever touched from the one
// decode task (the consumer's `read` loop, single-threaded per consumer).
unsafe impl Send for VideoToolbox {}

impl VideoToolbox {
	/// The VideoToolbox decode backend handles H.264 only for now; the selector
	/// never routes H.265 here, so `codec` is accepted for signature parity.
	pub(crate) fn open(_codec: Codec) -> Result<Box<dyn Backend>, Error> {
		tracing::info!(decoder = NAME, "opened H.264 decoder");
		Ok(Box::new(Self {
			session: None,
			format: None,
			sps: None,
			pps: None,
			built_from: None,
			sink: Box::new(Sink::default()),
		}))
	}

	/// (Re)build the decompression session when the parameter sets first appear
	/// or change. Returns `false` if we still don't have both SPS and PPS.
	fn ensure_session(&mut self, sps: Option<Bytes>, pps: Option<Bytes>) -> Result<bool, Error> {
		if let Some(sps) = sps {
			self.sps = Some(sps);
		}
		if let Some(pps) = pps {
			self.pps = Some(pps);
		}
		let (Some(sps), Some(pps)) = (self.sps.clone(), self.pps.clone()) else {
			return Ok(false);
		};

		// Reuse the existing session if it was built from these exact sets.
		if self.session.is_some() && self.built_from.as_ref() == Some(&(sps.clone(), pps.clone())) {
			return Ok(true);
		}

		let format = create_format_description(&sps, &pps)?;
		let attrs = nv12_output_attributes()?;

		let refcon = (&mut *self.sink as *mut Sink).cast::<c_void>();
		let record = VTDecompressionOutputCallbackRecord {
			decompressionOutputCallback: Some(output_callback),
			decompressionOutputRefCon: refcon,
		};

		let mut session_ptr: *mut VTDecompressionSession = ptr::null_mut();
		let status = unsafe {
			VTDecompressionSession::create(
				None,
				&format,
				None,
				Some(&attrs),
				&record,
				NonNull::new(&mut session_ptr).unwrap(),
			)
		};
		let session = NonNull::new(session_ptr)
			.filter(|_| status == 0)
			.map(|p| unsafe { CFRetained::from_raw(p) })
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("VTDecompressionSessionCreate failed: {status}")))?;

		self.session = Some(session);
		self.format = Some(format);
		self.built_from = Some((sps, pps));
		Ok(true)
	}
}

impl Backend for VideoToolbox {
	fn decode(&mut self, access_unit: Bytes, _keyframe: bool) -> Result<Vec<I420>, Error> {
		// Split the Annex-B access unit, pull out any parameter sets, and gather
		// the VCL slices into AVCC (4-byte length-prefixed) form. `NalIterator`
		// yields the parameter-set NALs as zero-copy `Bytes` (sub-slices of
		// `access_unit`), so SPS/PPS need no copy.
		let mut sps = None;
		let mut pps = None;
		let mut avcc: Vec<u8> = Vec::with_capacity(access_unit.len());
		let mut handle = |nal: Bytes| match nal_unit_type(&nal) {
			NAL_TYPE_SPS => sps = Some(nal),
			NAL_TYPE_PPS => pps = Some(nal),
			_ => {
				avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
				avcc.extend_from_slice(&nal);
			}
		};

		// `NalIterator` yields every NAL except the last (it has no trailing start
		// code); `flush` returns that final one.
		let mut buf = access_unit;
		let mut nals = NalIterator::new(&mut buf);
		for nal in nals.by_ref() {
			handle(nal.map_err(moq_mux::Error::from)?);
		}
		if let Some(nal) = nals.flush().map_err(moq_mux::Error::from)? {
			handle(nal);
		}

		if !self.ensure_session(sps, pps)? {
			// No parameter sets yet (e.g. a delta frame before the first keyframe).
			return Ok(Vec::new());
		}
		if avcc.is_empty() {
			// Parameter-set-only access unit: nothing to decode.
			return Ok(Vec::new());
		}

		let format = self.format.as_ref().expect("format ensured above");
		let sample = make_sample_buffer(&avcc, format)?;
		let session = self.session.as_ref().expect("session ensured above");

		self.sink.frames.clear();
		self.sink.error = None;

		let status = unsafe { session.decode_frame(&sample, VTDecodeFrameFlags(0), ptr::null_mut(), ptr::null_mut()) };
		if status != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"VTDecompressionSessionDecodeFrame failed: {status}"
			)));
		}

		if let Some(error) = self.sink.error.take() {
			return Err(Error::Codec(anyhow::anyhow!(
				"VideoToolbox decode callback failed: {error}"
			)));
		}
		Ok(std::mem::take(&mut self.sink.frames))
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// C callback VideoToolbox invokes (synchronously, from `decode_frame`) for each
/// decoded frame. Downloads the NV12 pixel buffer to packed I420.
unsafe extern "C-unwind" fn output_callback(
	refcon: *mut c_void,
	_source_frame_refcon: *mut c_void,
	status: i32,
	_flags: VTDecodeInfoFlags,
	image_buffer: *mut CVImageBuffer,
	_pts: CMTime,
	_duration: CMTime,
) {
	let sink = unsafe { &mut *(refcon as *mut Sink) };
	if status != 0 {
		sink.error = Some(format!("decode status {status}"));
		return;
	}
	let Some(image) = NonNull::new(image_buffer) else {
		return; // dropped frame
	};

	// The decoded image buffer is a CVPixelBuffer; retain it (the callback only
	// borrows) and download NV12 -> I420 with the shared capture-path code.
	let pixel_buffer = unsafe { CFRetained::retain(image.cast::<CVPixelBuffer>()) };
	let width = CVPixelBufferGetWidth(&pixel_buffer) as u32;
	let height = CVPixelBufferGetHeight(&pixel_buffer) as u32;
	let surface = crate::frame::macos::Surface::new(pixel_buffer, width, height);

	match surface.download_i420() {
		Ok(i420) => sink.frames.push(i420),
		Err(e) => sink.error = Some(e.to_string()),
	}
}

/// Build a `CMVideoFormatDescription` from raw SPS and PPS NAL units.
fn create_format_description(sps: &[u8], pps: &[u8]) -> Result<CFRetained<CMFormatDescription>, Error> {
	let pointers: [NonNull<u8>; 2] = [
		NonNull::new(sps.as_ptr() as *mut u8).ok_or_else(|| Error::Codec(anyhow::anyhow!("empty SPS")))?,
		NonNull::new(pps.as_ptr() as *mut u8).ok_or_else(|| Error::Codec(anyhow::anyhow!("empty PPS")))?,
	];
	let sizes: [usize; 2] = [sps.len(), pps.len()];

	let mut format_ptr: *const CMFormatDescription = ptr::null();
	let status = unsafe {
		CMVideoFormatDescriptionCreateFromH264ParameterSets(
			None,
			2,
			NonNull::new(pointers.as_ptr() as *mut NonNull<u8>).unwrap(),
			NonNull::new(sizes.as_ptr() as *mut usize).unwrap(),
			4, // 4-byte NAL length prefixes (AVCC), matching make_sample_buffer
			NonNull::new(&mut format_ptr).unwrap(),
		)
	};
	NonNull::new(format_ptr as *mut CMFormatDescription)
		.filter(|_| status == 0)
		.map(|p| unsafe { CFRetained::from_raw(p) })
		.ok_or_else(|| {
			Error::Codec(anyhow::anyhow!(
				"CMVideoFormatDescriptionCreateFromH264ParameterSets failed: {status}"
			))
		})
}

/// Wrap an AVCC (length-prefixed) access unit in a `CMSampleBuffer` for decode.
/// The block buffer owns a fresh copy of the bytes, so the sample outlives `avcc`.
fn make_sample_buffer(avcc: &[u8], format: &CMFormatDescription) -> Result<CFRetained<CMSampleBuffer>, Error> {
	let mut block_ptr: *mut CMBlockBuffer = ptr::null_mut();
	let status = unsafe {
		CMBlockBuffer::create_with_memory_block(
			None,
			ptr::null_mut(),
			avcc.len(),
			None,
			ptr::null(),
			0,
			avcc.len(),
			kCMBlockBufferAssureMemoryNowFlag,
			NonNull::new(&mut block_ptr).unwrap(),
		)
	};
	let block = NonNull::new(block_ptr)
		.filter(|_| status == 0)
		.map(|p| unsafe { CFRetained::from_raw(p) })
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("CMBlockBufferCreateWithMemoryBlock failed: {status}")))?;

	let status = unsafe {
		CMBlockBuffer::replace_data_bytes(
			NonNull::new(avcc.as_ptr() as *mut c_void).unwrap(),
			&block,
			0,
			avcc.len(),
		)
	};
	if status != 0 {
		return Err(Error::Codec(anyhow::anyhow!(
			"CMBlockBufferReplaceDataBytes failed: {status}"
		)));
	}

	let sizes: [usize; 1] = [avcc.len()];
	let mut sample_ptr: *mut CMSampleBuffer = ptr::null_mut();
	let status = unsafe {
		CMSampleBuffer::create_ready(
			None,
			Some(&block),
			Some(format),
			1,
			0,
			ptr::null(),
			1,
			sizes.as_ptr(),
			NonNull::new(&mut sample_ptr).unwrap(),
		)
	};
	NonNull::new(sample_ptr)
		.filter(|_| status == 0)
		.map(|p| unsafe { CFRetained::from_raw(p) })
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("CMSampleBufferCreateReady failed: {status}")))
}

/// Build the destination attributes requesting NV12 output, so the download path
/// (which expects NV12) always gets it regardless of the decoder's native format.
fn nv12_output_attributes() -> Result<CFRetained<CFDictionary>, Error> {
	let format = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange as i32;
	let number = unsafe { CFNumber::new(None, CFNumberType::SInt32Type, &format as *const i32 as *const c_void) }
		.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to build CFNumber")))?;

	let key = (unsafe { kCVPixelBufferPixelFormatTypeKey } as *const CFString).cast::<c_void>();
	let value = (number.as_ref() as *const CFNumber).cast::<c_void>();
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
	.ok_or_else(|| Error::Codec(anyhow::anyhow!("failed to build NV12 attributes dictionary")))
}

fn nal_unit_type(nal: &[u8]) -> u8 {
	nal.first().map_or(0, |b| b & 0x1f)
}
