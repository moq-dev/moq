//! Hardware H.264 / H.265 decode backend via Apple VideoToolbox
//! (`VTDecompressionSession`).
//!
//! The inverse of the encode VideoToolbox backend. We receive Annex-B access
//! units (parameter sets inline ahead of each keyframe: SPS/PPS for H.264,
//! VPS/SPS/PPS for H.265), so we:
//! - pull the parameter sets out of the stream and build a
//!   `CMVideoFormatDescription`, (re)creating the decompression session whenever
//!   they change;
//! - repackage the slice NALs as AVCC/HVCC (4-byte length-prefixed) in a
//!   `CMSampleBuffer`, the form VideoToolbox decodes;
//! - request NV12 output and hand the `CVPixelBuffer` back as-is, so a decoded
//!   frame stays GPU-resident. It is downloaded to I420 only when a consumer
//!   asks, via the same path the capture surfaces use.
//!
//! Hand-written on the raw `objc2-video-toolbox` bindings; there's no
//! higher-level crate we trust. Decoding is synchronous (no async flag), so the
//! output callback fires from within `decode_frame` before it returns, which is
//! what lets the `!Send` CoreFoundation handles stay thread-confined.

use std::ffi::c_void;
use std::ptr::{self, NonNull};

use bytes::Bytes;
use moq_mux::codec::annexb::NalIterator;
use moq_net::Timestamp;
use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CFRetained, CFString};
use objc2_core_media::{
	CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionCreateFromH264ParameterSets,
	CMVideoFormatDescriptionCreateFromHEVCParameterSets, kCMBlockBufferAssureMemoryNowFlag,
};
use objc2_core_video::{
	CVImageBuffer, CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetWidth, kCVPixelBufferPixelFormatTypeKey,
	kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
};
use objc2_video_toolbox::{
	VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord, VTDecompressionSession,
};

use super::{Backend, Codec, Config, Decoded};
use crate::Error;
use crate::frame::{Frame, macos::PixelBuffer};

pub(crate) const NAME: &str = "videotoolbox";

/// A parameter-set NAL we pull out of the stream to (re)build the format
/// description; `Slice` is everything else (the coded picture data we decode).
enum NalKind {
	Vps,
	Sps,
	Pps,
	Slice,
}

/// Where the C output callback drops decoded frames, drained after each
/// `decode_frame`. Boxed so its address is a stable refcon for the session.
#[derive(Default)]
struct Sink {
	frames: Vec<PixelBuffer>,
	error: Option<String>,
}

pub(crate) struct VideoToolbox {
	/// Which codec's parameter sets and format description to build (H.264 needs
	/// SPS+PPS; H.265 also needs VPS).
	codec: Codec,
	/// Built lazily once the parameter sets first arrive, rebuilt if they change.
	session: Option<CFRetained<VTDecompressionSession>>,
	/// Format description the current session + samples use (kept in lockstep
	/// with `session`).
	format: Option<CFRetained<CMFormatDescription>>,
	/// Latest parameter sets seen, persisted across access units (a delta frame
	/// carries none). `built_from` records the exact ordered set the live session
	/// was built from, so a mid-stream parameter-set change triggers a rebuild.
	vps: Option<Bytes>,
	sps: Option<Bytes>,
	pps: Option<Bytes>,
	built_from: Option<Vec<Bytes>>,
	sink: Box<Sink>,
}

// The session and its CoreFoundation handles are only ever touched from the one
// decode task (the consumer's `read` loop, single-threaded per consumer).
unsafe impl Send for VideoToolbox {}

impl VideoToolbox {
	/// Open a decoder for `codec` (H.264 or H.265). The session is built lazily
	/// once the first keyframe's parameter sets arrive.
	/// `config` is accepted for signature parity; VideoToolbox decodes at the
	/// stream's native size (callers scale the frames themselves).
	pub(crate) fn open(codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
		if codec == Codec::Av1 {
			return Err(Error::Codec(anyhow::anyhow!("VideoToolbox AV1 decode is not wired")));
		}
		tracing::info!(decoder = NAME, codec = ?codec, "opened video decoder");
		Ok(Box::new(Self {
			codec,
			session: None,
			format: None,
			vps: None,
			sps: None,
			pps: None,
			built_from: None,
			sink: Box::new(Sink::default()),
		}))
	}

	/// The ordered parameter sets the format description needs, or `None` if any
	/// required one hasn't been seen yet. H.264: `[SPS, PPS]`; H.265: `[VPS, SPS,
	/// PPS]`.
	fn param_sets(&self) -> Option<Vec<Bytes>> {
		let sps = self.sps.clone()?;
		let pps = self.pps.clone()?;
		match self.codec {
			Codec::H264 => Some(vec![sps, pps]),
			Codec::H265 => Some(vec![self.vps.clone()?, sps, pps]),
			Codec::Av1 => None,
		}
	}

	/// (Re)build the decompression session when the parameter sets first appear
	/// or change. Returns `false` if we still don't have a complete set.
	fn ensure_session(&mut self, vps: Option<Bytes>, sps: Option<Bytes>, pps: Option<Bytes>) -> Result<bool, Error> {
		if let Some(vps) = vps {
			self.vps = Some(vps);
		}
		if let Some(sps) = sps {
			self.sps = Some(sps);
		}
		if let Some(pps) = pps {
			self.pps = Some(pps);
		}
		let Some(params) = self.param_sets() else {
			return Ok(false);
		};

		// Reuse the existing session if it was built from these exact sets.
		if self.session.is_some() && self.built_from.as_ref() == Some(&params) {
			return Ok(true);
		}

		let format = create_format_description(self.codec, &params)?;
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
		self.built_from = Some(params);
		Ok(true)
	}
}

impl Backend for VideoToolbox {
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Decoded>, Error> {
		// Split the Annex-B access unit, pull out any parameter sets, and gather
		// the slices into length-prefixed (4-byte) form. `NalIterator` yields the
		// parameter-set NALs as zero-copy `Bytes` (sub-slices of `access_unit`), so
		// they need no copy.
		let codec = self.codec;
		let mut vps = None;
		let mut sps = None;
		let mut pps = None;
		let mut avcc: Vec<u8> = Vec::with_capacity(access_unit.len());
		let mut handle = |nal: Bytes| match nal_kind(&nal, codec) {
			NalKind::Vps => vps = Some(nal),
			NalKind::Sps => sps = Some(nal),
			NalKind::Pps => pps = Some(nal),
			NalKind::Slice => {
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

		if !self.ensure_session(vps, sps, pps)? {
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
		// The decode callback fires synchronously inside `decode_frame`, so
		// every output frame belongs to the access unit just submitted.
		Ok(std::mem::take(&mut self.sink.frames)
			.into_iter()
			.map(|surface| Decoded {
				timestamp,
				frame: Frame::PixelBuffer(surface),
			})
			.collect())
	}

	fn name(&self) -> &str {
		NAME
	}
}

/// C callback VideoToolbox invokes (synchronously, from `decode_frame`) for each
/// decoded frame. Retains the NV12 pixel buffer so the picture stays on the GPU.
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
	// borrows) and keep it as-is rather than downloading here. The retain is also
	// what stops VideoToolbox handing this buffer back out of its pool while a
	// consumer still holds the frame. The flip side: a consumer that hoards frames
	// holds pool buffers, so the pool (not CPU memory) is the pressure point now.
	let pixel_buffer = unsafe { CFRetained::retain(image.cast::<CVPixelBuffer>()) };
	let width = CVPixelBufferGetWidth(&pixel_buffer) as u32;
	let height = CVPixelBufferGetHeight(&pixel_buffer) as u32;

	sink.frames.push(PixelBuffer::new(pixel_buffer, width, height));
}

/// Build a `CMVideoFormatDescription` from the ordered parameter-set NAL units
/// (`[SPS, PPS]` for H.264; `[VPS, SPS, PPS]` for H.265).
fn create_format_description(codec: Codec, params: &[Bytes]) -> Result<CFRetained<CMFormatDescription>, Error> {
	let pointers: Vec<NonNull<u8>> = params
		.iter()
		.map(|p| {
			NonNull::new(p.as_ptr() as *mut u8).ok_or_else(|| Error::Codec(anyhow::anyhow!("empty parameter set")))
		})
		.collect::<Result<_, _>>()?;
	let sizes: Vec<usize> = params.iter().map(|p| p.len()).collect();
	let count = params.len();
	// `pointers` / `sizes` must outlive the call below; keep them named so they're
	// not dropped while the C function reads through these raw pointers.
	let pointers_ptr = NonNull::new(pointers.as_ptr() as *mut NonNull<u8>).unwrap();
	let sizes_ptr = NonNull::new(sizes.as_ptr() as *mut usize).unwrap();

	let mut format_ptr: *const CMFormatDescription = ptr::null();
	// 4-byte NAL length prefixes (AVCC/HVCC), matching make_sample_buffer.
	let status = match codec {
		Codec::H264 => unsafe {
			CMVideoFormatDescriptionCreateFromH264ParameterSets(
				None,
				count,
				pointers_ptr,
				sizes_ptr,
				4,
				NonNull::new(&mut format_ptr).unwrap(),
			)
		},
		Codec::H265 => unsafe {
			CMVideoFormatDescriptionCreateFromHEVCParameterSets(
				None,
				count,
				pointers_ptr,
				sizes_ptr,
				4,
				None, // no extensions
				NonNull::new(&mut format_ptr).unwrap(),
			)
		},
		Codec::Av1 => {
			return Err(Error::Codec(anyhow::anyhow!("VideoToolbox AV1 decode is not wired")));
		}
	};
	NonNull::new(format_ptr as *mut CMFormatDescription)
		.filter(|_| status == 0)
		.map(|p| unsafe { CFRetained::from_raw(p) })
		.ok_or_else(|| {
			Error::Codec(anyhow::anyhow!(
				"CMVideoFormatDescriptionCreateFrom*ParameterSets failed: {status}"
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

/// Classify a NAL by its header so the parameter sets can be split out. H.264
/// carries the type in the low 5 bits of one header byte (SPS 7, PPS 8); H.265
/// uses bits 1..=6 of a two-byte header (VPS 32, SPS 33, PPS 34).
fn nal_kind(nal: &[u8], codec: Codec) -> NalKind {
	let Some(&b) = nal.first() else {
		return NalKind::Slice;
	};
	match codec {
		Codec::H264 => match b & 0x1f {
			7 => NalKind::Sps,
			8 => NalKind::Pps,
			_ => NalKind::Slice,
		},
		Codec::H265 => match (b >> 1) & 0x3f {
			32 => NalKind::Vps,
			33 => NalKind::Sps,
			34 => NalKind::Pps,
			_ => NalKind::Slice,
		},
		Codec::Av1 => NalKind::Slice,
	}
}
