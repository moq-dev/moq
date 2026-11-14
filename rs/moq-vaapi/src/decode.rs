// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// Adapted from discord/cros-codecs (`decoder/stateless/h264.rs` and
// `decoder/stateless/h264/vaapi.rs`), itself BSD-3-Clause / Copyright The
// ChromiumOS Authors. See LICENSE.cros-codecs.

//! Thin VA-API H.264 and H.265 decoder built on the vendored libva binding.
//!
//! The mirror of [`crate::encode`]. It reuses the backend-agnostic bitstream
//! layers ([`crate::codec::h264`], [`crate::codec::h265`]) from
//! discord/cros-codecs (BSD-3-Clause) for parameter set and slice parsing,
//! reference marking, and DPB bumping, then drives libva directly rather than
//! vendoring cros-codecs's generic multi-backend decoder framework. The
//! per-picture and per-slice VA buffer population is ported from cros-codecs's
//! `decoder/stateless/h264/vaapi.rs` and, in `h265.rs`, from
//! `decoder/stateless/h265.rs` and `decoder/stateless/h265/vaapi.rs`.
//!
//! Feed [`Decoder::decode`] one Annex-B access unit at a time. Its picture is
//! submitted and completed before the call returns, rather than when the next
//! unit's first slice arrives, so the hardware is never left holding a
//! half-submitted picture between calls.
//!
//! Output trails input by the reorder depth the SPS declares. H.264 bumps
//! the DPB when a new picture needs a frame buffer (C.4.5.3), and also lets a
//! picture out as soon as more pictures wait than the SPS's reorder limit
//! allows, so a stream that declares no reordering comes out as it decodes
//! rather than one picture behind. An SPS without `bitstream_restriction`
//! declares nothing, so E.2.1 infers the DPB size and such a stream still
//! trails by it. H.265 bumps by the SPS's reorder limit (C.5.2) to the same
//! effect. A stream with B-frames still holds pictures back, so [`Frame`]
//! carries the timestamp of the access unit it was coded in, and
//! [`Decoder::flush`] is what releases the tail at the end of a stream.
//!
//! Progressive 8-bit 4:2:0 only: a sequence that is interlaced, deeper than 8
//! bits, or not 4:2:0 is rejected rather than decoded wrongly. That covers every
//! stream this crate's encoder, WebRTC, or a browser's `VideoEncoder` produces.
//! H.265 is the Main profile, with Main 10 and Main Still Picture streams that
//! hold 8-bit samples decoded the same way. Left and top cropping are rejected;
//! right and bottom cropping are supported.

use std::io::Cursor;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context as _};

use crate::codec::h264::dpb::{Dpb, DpbEntry, DpbPicRefList, MmcoError, ReferencePicLists};
use crate::codec::h264::parser::{
	Level, MaxLongTermFrameIdx, Nalu, NaluType, Parser, Pps, Profile, RefPicListModification, Slice, SliceHeader,
	SliceType, Sps,
};
use crate::codec::h264::picture::{Field, IsIdr, PictureData, Reference};
use crate::codec::Codec;
use crate::pool::{PooledSurface, SurfacePool};
use crate::{
	bindings, Buffer, BufferType, Config as VaConfig, Context, Display, DrmPrimeSurfaceDescriptor, H264PicFields,
	H264SeqFields, IQMatrix, IQMatrixBufferH264, Image, Picture, PictureH264, PictureParameter,
	PictureParameterBufferH264, SliceParameter, SliceParameterBufferH264, Surface, UsageHint, VAConfigAttrib,
	VAConfigAttribType, VAEntrypoint, VAProfile, VA_FOURCC_NV12, VA_INVALID_ID, VA_PICTURE_H264_INVALID,
	VA_PICTURE_H264_LONG_TERM_REFERENCE, VA_PICTURE_H264_SHORT_TERM_REFERENCE, VA_RT_FORMAT_YUV420,
	VA_SLICE_DATA_FLAG_ALL,
};

mod h265;

/// Decoder configuration.
#[derive(Clone, Debug)]
pub struct Config {
	/// The format to decode, H.264 unless set.
	pub codec: Codec,
	/// DRM render node to open (e.g. `/dev/dri/renderD128`).
	///
	/// `None`, the default, opens the first node whose driver can decode
	/// [`codec`](Self::codec).
	pub device: Option<PathBuf>,
}

impl Config {
	/// Returns an H.264 configuration that opens the first render node whose driver can decode it.
	pub fn new() -> Self {
		Self {
			codec: Codec::H264,
			device: None,
		}
	}
}

impl Default for Config {
	fn default() -> Self {
		Self::new()
	}
}

/// One decoded picture, downloaded to client memory as tightly packed NV12.
#[derive(Clone)]
pub struct Frame {
	/// Timestamp of the access unit this picture was coded in, as handed to
	/// [`Decoder::decode`].
	pub timestamp: u64,
	/// Visible width in pixels, i.e. after the SPS cropping rectangle.
	pub width: u32,
	/// Visible height in pixels.
	pub height: u32,
	/// NV12 planes with no row padding: `width * height` luma bytes followed by
	/// `width * height / 2` interleaved chroma bytes.
	pub data: Vec<u8>,
}

/// One decoded picture, left in the surface the hardware wrote it into.
///
/// The GPU counterpart of [`Frame`]: same picture, same timestamp, but a DRM
/// PRIME descriptor for the surface rather than a copy of its pixels. The
/// descriptor owns the exported file descriptors and the surface travels with
/// it, so both the allocation and libva's handle on it outlive the decoder.
///
/// The export is NV12 in one object holding both planes, which is what the
/// Intel and AMD drivers produce. Read the modifier off the object before
/// assuming anything about the layout: a decode target is tiled, and the plane
/// pitches carry padding the visible size does not imply. That is also why
/// [`ExportedFrame::download`] goes back through the surface instead of reading
/// the file descriptor as rows.
///
/// Dropping the frame hands its surface back to the decoder, which decodes a
/// later picture into it. So keep the frame for as long as anything reads the
/// descriptor or an import of it: a GPU consumer holds it until the work that
/// samples it has finished, not merely until that work is submitted. A frame
/// that outlives its decoder, or the sequence it was decoded in, releases its
/// surface to the driver instead.
///
/// This is [`Send`] and [`Sync`] even though [`Decoder`] is neither: a surface
/// is a display and an id, both of which libva serializes internally, and none
/// of the decoder's `Rc`s come along.
pub struct ExportedFrame {
	/// Timestamp of the access unit this picture was coded in, as handed to
	/// [`Decoder::decode_exported`].
	pub timestamp: u64,
	/// Visible width in pixels, i.e. after the SPS cropping rectangle.
	pub width: u32,
	/// Visible height in pixels.
	pub height: u32,
	/// The surface's export: format, modifier, and the offset and pitch of each
	/// plane.
	pub descriptor: DrmPrimeSurfaceDescriptor,
	/// The surface the picture was decoded into, held here so the decoder does
	/// not decode into it again until this drops, and so
	/// [`ExportedFrame::download`] has something to map.
	surface: Arc<PooledSurface>,
}

impl ExportedFrame {
	/// Exports `surface`, an NV12 picture from somewhere other than the decoder, the way the decoder exports its own.
	///
	/// A picture the video processor produced, such as a scaled copy of a
	/// captured frame, then reaches its consumer with the same descriptor and
	/// the same [`download`](Self::download) fallback as a decoded one, so one
	/// consumer handles both. `timestamp` is carried through untouched.
	///
	/// # Errors
	///
	/// Fails when the surface is not NV12, cannot be synced, or the driver will
	/// not export it.
	pub fn from_surface(surface: Surface<()>, timestamp: u64) -> anyhow::Result<Self> {
		let size = surface.size();
		Self::export(Arc::new(PooledSurface::unpooled(surface)), timestamp, size)
	}

	/// Exports `surface`, whose picture is the top-left `width` by `height` of it, copying nothing.
	///
	/// The frame holds a reference to the surface, so a pool does not hand it
	/// out again while the caller holds the descriptor, and
	/// [`download`](Self::download) can still map it.
	pub(crate) fn export(
		surface: Arc<PooledSurface>,
		timestamp: u64,
		(width, height): (u32, u32),
	) -> anyhow::Result<Self> {
		surface.get().sync().map_err(|e| anyhow!("surface sync: {e:?}"))?;
		let descriptor = surface
			.get()
			.export_prime()
			.map_err(|e| anyhow!("export the surface: {e:?}"))?;
		if descriptor.fourcc != VA_FOURCC_NV12 {
			anyhow::bail!("exported fourcc {:#010x}, expected NV12", descriptor.fourcc);
		}
		Ok(Self {
			timestamp,
			width,
			height,
			descriptor,
			surface,
		})
	}

	/// Reads this picture back to client memory as tightly packed NV12.
	///
	/// The way back for a caller that took a descriptor and then found itself
	/// with a consumer that wants bytes. It costs the read [`Decoder::decode`]
	/// would have done up front, so prefer that entry point when nothing on the
	/// path draws on the GPU.
	///
	/// Same layout as [`Frame::data`], with the driver's row padding dropped.
	pub fn download(&self) -> anyhow::Result<Frame> {
		read_back(self.surface.get(), self.timestamp, self.width, self.height)
	}
}

impl std::fmt::Debug for ExportedFrame {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("ExportedFrame")
			.field("timestamp", &self.timestamp)
			.field("width", &self.width)
			.field("height", &self.height)
			.field("fourcc", &self.descriptor.fourcc)
			.finish()
	}
}

impl std::fmt::Debug for Frame {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Frame")
			.field("timestamp", &self.timestamp)
			.field("width", &self.width)
			.field("height", &self.height)
			.field("bytes", &self.data.len())
			.finish()
	}
}

/// A VA-API H.264 or H.265 decoder. Built once, fed Annex-B access units, emits NV12.
///
/// # Examples
///
/// ```no_run
/// use moq_vaapi::decode::{Config, Decoder};
///
/// # fn main() -> anyhow::Result<()> {
/// # let access_units: Vec<Vec<u8>> = Vec::new();
/// let mut decoder = Decoder::new(Config::new())?;
/// let mut pictures = Vec::new();
///
/// for (index, access_unit) in access_units.iter().enumerate() {
///     pictures.extend(decoder.decode(access_unit, index as u64)?);
/// }
///
/// // Output trails input, so the tail of the stream only arrives here.
/// pictures.extend(decoder.flush()?);
/// # Ok(())
/// # }
/// ```
pub struct Decoder {
	inner: Inner,
}

/// The decoder for each format. They share the VA plumbing: [`Sequence`],
/// [`Handle`], and the read-back and export of a decoded picture.
enum Inner {
	H264(H264),
	H265(h265::Decoder),
}

/// The H.264 decoder.
struct H264 {
	parser: Parser,
	dpb: Dpb<Handle>,
	/// Cached variables from the previous reference picture.
	prev_ref_pic_info: PrevReferencePicInfo,
	/// Cached variables from the previous picture.
	prev_pic_info: PrevPicInfo,
	max_long_term_frame_idx: MaxLongTermFrameIdx,
	/// VA state for the active sequence, rebuilt when the SPS changes something
	/// the config, context, or surface pool depends on.
	sequence: Option<Sequence>,
	/// Pictures bumped out of the DPB, in output order, awaiting download.
	ready: Vec<Handle>,
	/// Drop order: everything above borrows the display.
	display: Arc<Display>,
}

impl Decoder {
	/// Opens [`Config::device`], or the first render node that decodes
	/// [`Config::codec`], and checks that the driver exposes a decode
	/// entrypoint for it.
	///
	/// The VA config, context, and surface pool are built later, from the first
	/// SPS, since their profile and size come from the stream. This still fails
	/// early enough for a caller to fall back to another decoder when libva is
	/// missing, the node cannot be opened, or the driver does not decode the
	/// codec. A named node is never swapped for another.
	pub fn new(config: Config) -> anyhow::Result<Self> {
		let codec = config.codec;
		let (device, display) = match config.device {
			Some(device) => {
				let display =
					Display::open_drm_display(&device).map_err(|e| anyhow!("open DRM display {device:?}: {e:?}"))?;
				probe(&display, codec)?;
				(device, display)
			}
			None => crate::display::open_first(|display| probe(display, codec))
				.map_err(|e| e.context(format!("find a render node that decodes {codec:?}")))?,
		};

		tracing::info!("opened VA-API {codec:?} decoder on {device:?}");
		let inner = match codec {
			Codec::H264 => Inner::H264(H264::new(display)),
			Codec::H265 => Inner::H265(h265::Decoder::new(display)),
		};
		Ok(Self { inner })
	}

	/// Decodes one Annex-B access unit, returning the pictures that came due, in
	/// output order.
	///
	/// `timestamp` is opaque and travels with the picture coded in this access
	/// unit, so it survives DPB reordering. Parameter sets are read from the
	/// stream, so an access unit carrying only an SPS and a PPS decodes to no
	/// frames.
	pub fn decode(&mut self, access_unit: &[u8], timestamp: u64) -> anyhow::Result<Vec<Frame>> {
		self.submit(access_unit, timestamp)?;
		self.take_frames()
	}

	/// Decodes one Annex-B access unit, leaving the pictures that came due on
	/// the GPU.
	///
	/// The same decode as [`Decoder::decode`], differing only in what the caller
	/// gets back: a DRM PRIME descriptor for each picture's surface instead of a
	/// copy of its pixels. For a consumer that draws on the GPU that is the
	/// whole picture never touching system memory.
	///
	/// An exported surface stays out of the recycling pool until the
	/// [`ExportedFrame`] drops, since the next picture would otherwise be decoded
	/// over pixels the consumer still holds a descriptor for. A consumer that
	/// lets each picture go once it has drawn it therefore costs no allocation
	/// per picture; one that holds on to pictures costs one surface per picture
	/// held.
	///
	/// A consumer that turns out to want bytes after all is not stuck: the
	/// surface travels with the descriptor, and [`ExportedFrame::download`] reads
	/// it back.
	pub fn decode_exported(&mut self, access_unit: &[u8], timestamp: u64) -> anyhow::Result<Vec<ExportedFrame>> {
		self.submit(access_unit, timestamp)?;
		self.take_exported()
	}

	/// Decodes one access unit, leaving the pictures that came due in the ready
	/// queue for whichever of [`Decoder::take_frames`] or
	/// [`Decoder::take_exported`] the caller asked for.
	fn submit(&mut self, access_unit: &[u8], timestamp: u64) -> anyhow::Result<()> {
		let nalus = match &mut self.inner {
			Inner::H264(decoder) => decoder.submit(access_unit, timestamp)?,
			Inner::H265(decoder) => decoder.submit(access_unit, timestamp)?,
		};

		// A decoder cannot tell a truncated unit from a complete one, so a
		// caller that hands us the wrong container gets silence. Say so once per
		// unit rather than leaving them to wonder where their pictures went.
		if nalus == 0 && !access_unit.is_empty() {
			tracing::warn!(
				"access unit of {} bytes holds no Annex-B NAL unit; length-prefixed input has to be converted first",
				access_unit.len()
			);
		}
		Ok(())
	}

	/// Returns every picture still held in the DPB, in output order.
	///
	/// The DPB lets a picture out only when a later one needs it to (C.4.5.3
	/// for H.264, C.5.2 for H.265), so a stream that simply stops leaves its
	/// tail sitting there. This releases that tail and empties the DPB, so a
	/// second call returns nothing.
	///
	/// The parsed parameter sets survive, and nothing here waits for an IDR: a
	/// stream picked up part way through decodes against an empty DPB, which
	/// gives distorted pictures until its next IDR rather than an error.
	pub fn flush(&mut self) -> anyhow::Result<Vec<Frame>> {
		self.reset();
		self.take_frames()
	}

	/// [`Decoder::flush`] for a caller taking its pictures on the GPU.
	pub fn flush_exported(&mut self) -> anyhow::Result<Vec<ExportedFrame>> {
		self.reset();
		self.take_exported()
	}

	/// Moves the DPB to the ready queue and forgets the state the next picture
	/// would otherwise be derived against.
	fn reset(&mut self) {
		match &mut self.inner {
			Inner::H264(decoder) => decoder.reset(),
			Inner::H265(decoder) => decoder.reset(),
		}
	}

	/// The pictures bumped out of the DPB, in output order, awaiting download.
	fn ready(&mut self) -> &mut Vec<Handle> {
		match &mut self.inner {
			Inner::H264(decoder) => &mut decoder.ready,
			Inner::H265(decoder) => &mut decoder.ready,
		}
	}

	/// Downloads every picture waiting in the ready queue.
	fn take_frames(&mut self) -> anyhow::Result<Vec<Frame>> {
		std::mem::take(self.ready()).iter().map(download).collect()
	}

	/// Exports every picture waiting in the ready queue, leaving the pixels
	/// where the hardware wrote them.
	fn take_exported(&mut self) -> anyhow::Result<Vec<ExportedFrame>> {
		std::mem::take(self.ready()).iter().map(export).collect()
	}

	/// The VA state of the active sequence, for tests that look at its pool.
	#[cfg(test)]
	fn sequence(&self) -> Option<&Sequence> {
		match &self.inner {
			Inner::H264(decoder) => decoder.sequence.as_ref(),
			Inner::H265(decoder) => decoder.sequence.as_ref(),
		}
	}
}

impl H264 {
	fn new(display: Arc<Display>) -> Self {
		Self {
			parser: Parser::default(),
			dpb: Dpb::default(),
			prev_ref_pic_info: PrevReferencePicInfo::default(),
			prev_pic_info: PrevPicInfo::default(),
			max_long_term_frame_idx: MaxLongTermFrameIdx::default(),
			sequence: None,
			ready: Vec::new(),
			display,
		}
	}

	/// Decodes one access unit into the ready queue, returning how many NAL units it held.
	fn submit(&mut self, access_unit: &[u8], timestamp: u64) -> anyhow::Result<usize> {
		let mut cursor = Cursor::new(access_unit);
		let mut current: Option<CurrentPicture> = None;
		let mut nalus = 0usize;

		// `Nalu::next` reports the end of the buffer as an error, so a truncated
		// unit is indistinguishable from a complete one and simply stops us.
		while let Ok(nalu) = Nalu::next(&mut cursor) {
			nalus += 1;
			match nalu.header.type_ {
				NaluType::Sps => {
					self.parser.parse_sps(&nalu).map_err(|e| anyhow!("parse SPS: {e}"))?;
				}
				NaluType::Pps => {
					self.parser.parse_pps(&nalu).map_err(|e| anyhow!("parse PPS: {e}"))?;
				}
				NaluType::Slice
				| NaluType::SliceDpa
				| NaluType::SliceDpb
				| NaluType::SliceDpc
				| NaluType::SliceIdr
				| NaluType::SliceExt => {
					let slice = self
						.parser
						.parse_slice_header(nalu)
						.map_err(|e| anyhow!("parse slice header: {e}"))?;

					let mut picture = match current.take() {
						// An access unit normally holds one picture, but nothing
						// forces that: `first_mb_in_slice == 0` starts a new one,
						// so the picture in flight is done.
						Some(picture) if slice.header.first_mb_in_slice == 0 => {
							self.finish_picture(picture)?;
							self.begin_picture(&slice, timestamp)?
						}
						Some(picture) => picture,
						None => self.begin_picture(&slice, timestamp)?,
					};

					self.decode_slice(&mut picture, &slice)?;
					current = Some(picture);
				}
				other => tracing::trace!("ignoring NAL unit of type {other:?}"),
			}
		}

		// Finishing here rather than on the next unit's first slice keeps the
		// hardware from holding a half-submitted picture between calls.
		if let Some(picture) = current.take() {
			self.finish_picture(picture)?;
		}

		Ok(nalus)
	}

	/// Moves the DPB to the ready queue and forgets the order count state the
	/// next picture would otherwise be derived against.
	fn reset(&mut self) {
		self.drain();
		self.prev_ref_pic_info = PrevReferencePicInfo::default();
		self.prev_pic_info = PrevPicInfo::default();
		self.max_long_term_frame_idx = MaxLongTermFrameIdx::default();
	}

	/// Moves everything left in the DPB to the ready queue.
	fn drain(&mut self) {
		self.ready.extend(self.dpb.drain().into_iter().flatten());
	}

	/// Applies `sps` to the DPB limits, rebuilding the VA config, context, and
	/// surface pool if it changes anything they depend on.
	fn apply_sps(&mut self, sps: &Sps) -> anyhow::Result<()> {
		let info = SequenceInfo::new(sps)?;
		// C.4.5.3: the DPB limits follow the active SPS whether or not the VA
		// state has to be rebuilt.
		self.dpb.set_limits(sps.max_dpb_frames(), reorder_limit(sps));

		let format = Format {
			codec: Codec::H264,
			profile: va_profile(sps)?,
			coded: info.coded,
			visible: info.visible,
			max_dpb_frames: info.max_dpb_frames,
		};
		if self.sequence.as_ref().map(|sequence| &sequence.format) == Some(&format) {
			return Ok(());
		}

		// Surfaces are about to change shape, so let everything decoded against
		// the old sequence out first. Those handles keep their own surfaces (and
		// through them the old pool) alive until they are downloaded.
		self.drain();

		self.sequence = Some(Sequence::new(&self.display, format)?);
		Ok(())
	}

	/// Starts a picture: computes its order count, updates the DPB's picture
	/// numbers, and hands the driver the picture parameters.
	fn begin_picture(&mut self, slice: &Slice, timestamp: u64) -> anyhow::Result<CurrentPicture> {
		let pps = Rc::clone(
			self.parser
				.get_pps(slice.header.pic_parameter_set_id)
				.context("slice refers to an unknown PPS")?,
		);
		let sps = Rc::clone(&pps.sps);
		self.apply_sps(&sps)?;

		if slice.nalu.header.idr_pic_flag {
			self.prev_ref_pic_info.frame_num = 0;
		}

		let frame_num = u32::from(slice.header.frame_num);
		if frame_num != self.prev_ref_pic_info.frame_num
			&& frame_num != (self.prev_ref_pic_info.frame_num + 1) % sps.max_frame_num()
		{
			self.handle_frame_num_gap(&sps, frame_num, timestamp)?;
		}

		let pic = self.init_current_pic(slice, &sps, timestamp)?;
		let ref_pic_lists = self.dpb.build_ref_pic_lists(&pic);

		// `apply_sps` above either kept the sequence or built a new one.
		let sequence = self.sequence.as_ref().expect("sequence built by apply_sps");
		let context = Rc::clone(&sequence.context);
		let handle = sequence.handle(timestamp)?;

		let mut picture = Picture::new(timestamp, Rc::clone(&context), handle.clone());
		let pic_param = build_pic_param(&slice.header, &pic, handle.surface.id(), &self.dpb, &sps, &pps);
		picture.add_buffer(create_buffer(&context, pic_param)?);
		picture.add_buffer(create_buffer(&context, build_iq_matrix(&pps))?);

		tracing::trace!("starting picture POC {}", pic.pic_order_cnt);
		Ok(CurrentPicture {
			pic,
			pps,
			handle,
			picture,
			ref_pic_lists,
		})
	}

	/// Hands the driver one slice: its parameters, its reference lists, and the
	/// slice NAL itself.
	fn decode_slice(&mut self, current: &mut CurrentPicture, slice: &Slice) -> anyhow::Result<()> {
		// A VA slice parameter carries both of these in 16 bits, so refuse what
		// would otherwise be truncated into a plausible wrong offset. Only a
		// picture past 65536 macroblocks, which is beyond 8K, gets close.
		if slice.header.first_mb_in_slice > u32::from(u16::MAX) {
			bail!(
				"slice starts at macroblock {}, past what a VA slice parameter addresses",
				slice.header.first_mb_in_slice
			);
		}
		if slice.header.header_bit_size > usize::from(u16::MAX) {
			bail!(
				"slice header is {} bits long, past what a VA slice parameter addresses",
				slice.header.header_bit_size
			);
		}

		// A slice may refer to a different PPS than the one that started the
		// picture, as long as it names the same SPS.
		let pps = Rc::clone(
			self.parser
				.get_pps(slice.header.pic_parameter_set_id)
				.context("slice refers to an unknown PPS")?,
		);
		if SequenceInfo::new(&pps.sps)? != SequenceInfo::new(&current.pps.sps)? {
			bail!("invalid stream: the sequence changed between slices of one picture");
		}
		current.pps = Rc::clone(&pps);

		let (list0, list1) = self.create_ref_pic_lists(&current.pic, &slice.header, &current.ref_pic_lists)?;
		let context = &self.sequence.as_ref().expect("sequence built by apply_sps").context;

		let slice_param = build_slice_param(&slice.header, slice.nalu.size, &list0, &list1, &pps.sps, &pps);
		current.picture.add_buffer(create_buffer(context, slice_param)?);
		current.picture.add_buffer(create_buffer(
			context,
			BufferType::SliceData(slice.nalu.as_ref().to_vec()),
		)?);

		Ok(())
	}

	/// Submits the picture to the hardware, then runs the reference marking and
	/// bumping the specification calls for once a picture is decoded.
	fn finish_picture(&mut self, current: CurrentPicture) -> anyhow::Result<()> {
		let CurrentPicture {
			mut pic,
			pps,
			handle,
			picture,
			..
		} = current;

		run(picture)?;
		tracing::trace!("decoded picture POC {}", pic.pic_order_cnt);

		if pic.is_ref() {
			self.reference_pic_marking(&mut pic, &pps.sps)?;
			self.prev_ref_pic_info.fill(&pic);
		}
		self.prev_pic_info.fill(&pic);

		// C.4.5.3 clause 3: a picture with memory_management_control_operation
		// equal to 5 empties the DPB.
		if pic.has_mmco_5 {
			self.drain();
		}

		// C.4.4 and C.4.5: free what the marking released, then store or output.
		self.dpb
			.finish_picture(pic, handle, &mut self.ready)
			.map_err(|e| anyhow!("store picture in DPB: {e}"))?;

		Ok(())
	}

	/// Builds the picture for `slice` and makes room for it in the DPB.
	fn init_current_pic(&mut self, slice: &Slice, sps: &Sps, timestamp: u64) -> anyhow::Result<PictureData> {
		// Progressive only, so a picture is never the second field of a frame.
		let mut pic = PictureData::new_from_slice(slice, sps, timestamp, None);
		self.compute_pic_order_count(&mut pic, sps)?;

		if matches!(pic.is_idr, IsIdr::Yes { .. }) {
			// C.4.5.3 clause 2: an IDR bumps the whole DPB, unless
			// no_output_of_prior_pics_flag says to discard it instead (C.4.4).
			if pic.ref_pic_marking.no_output_of_prior_pics_flag {
				self.dpb.clear();
			} else {
				self.drain();
			}
		}

		self.dpb
			.update_pic_nums(u32::from(slice.header.frame_num), sps.max_frame_num(), &pic);

		Ok(pic)
	}

	/// 8.2.5.2: invents the pictures for the frame numbers a lossy stream skipped,
	/// so reference marking keeps working.
	fn handle_frame_num_gap(&mut self, sps: &Sps, frame_num: u32, timestamp: u64) -> anyhow::Result<()> {
		if self.dpb.is_empty() {
			return Ok(());
		}

		if !sps.gaps_in_frame_num_value_allowed_flag {
			bail!("invalid frame_num {frame_num}: assuming unintentional loss of pictures");
		}
		tracing::debug!("frame_num gap up to {frame_num}");

		let max_frame_num = sps.max_frame_num();
		let mut unused = (self.prev_ref_pic_info.frame_num + 1) % max_frame_num;
		while unused != frame_num {
			let mut pic = PictureData::new_non_existing(unused, timestamp);
			self.compute_pic_order_count(&mut pic, sps)?;

			self.dpb.update_pic_nums(unused, max_frame_num, &pic);
			self.dpb.sliding_window_marking(&mut pic, sps);
			self.ready.extend(self.dpb.bump_as_needed(&pic).into_iter().flatten());
			self.dpb
				.store_picture(pic.into_rc(), None)
				.map_err(|e| anyhow!("store non-existing picture in DPB: {e}"))?;

			unused = (unused + 1) % max_frame_num;
		}

		Ok(())
	}

	/// 8.2.1: derives the picture order count, which drives output ordering.
	fn compute_pic_order_count(&mut self, pic: &mut PictureData, sps: &Sps) -> anyhow::Result<()> {
		match pic.pic_order_cnt_type {
			// 8.2.1.1
			0 => {
				let (prev_pic_order_cnt_msb, prev_pic_order_cnt_lsb) = if matches!(pic.is_idr, IsIdr::Yes { .. }) {
					(0, 0)
				} else if self.prev_ref_pic_info.has_mmco_5 {
					if matches!(self.prev_ref_pic_info.field, Field::Bottom) {
						(0, 0)
					} else {
						(0, self.prev_ref_pic_info.top_field_order_cnt)
					}
				} else {
					(
						self.prev_ref_pic_info.pic_order_cnt_msb,
						self.prev_ref_pic_info.pic_order_cnt_lsb,
					)
				};

				let max_pic_order_cnt_lsb = 1 << (sps.log2_max_pic_order_cnt_lsb_minus4 + 4);

				pic.pic_order_cnt_msb = if (pic.pic_order_cnt_lsb < prev_pic_order_cnt_lsb)
					&& (prev_pic_order_cnt_lsb - pic.pic_order_cnt_lsb >= max_pic_order_cnt_lsb / 2)
				{
					prev_pic_order_cnt_msb + max_pic_order_cnt_lsb
				} else if (pic.pic_order_cnt_lsb > prev_pic_order_cnt_lsb)
					&& (pic.pic_order_cnt_lsb - prev_pic_order_cnt_lsb > max_pic_order_cnt_lsb / 2)
				{
					prev_pic_order_cnt_msb - max_pic_order_cnt_lsb
				} else {
					prev_pic_order_cnt_msb
				};

				if !matches!(pic.field, Field::Bottom) {
					pic.top_field_order_cnt = pic.pic_order_cnt_msb + pic.pic_order_cnt_lsb;
				}

				if !matches!(pic.field, Field::Top) {
					pic.bottom_field_order_cnt = if matches!(pic.field, Field::Frame) {
						pic.top_field_order_cnt + pic.delta_pic_order_cnt_bottom
					} else {
						pic.pic_order_cnt_msb + pic.pic_order_cnt_lsb
					};
				}
			}

			// 8.2.1.2
			1 => {
				if self.prev_pic_info.has_mmco_5 {
					self.prev_pic_info.frame_num_offset = 0;
				}

				pic.frame_num_offset = if matches!(pic.is_idr, IsIdr::Yes { .. }) {
					0
				} else if self.prev_pic_info.frame_num > pic.frame_num {
					self.prev_pic_info.frame_num_offset + sps.max_frame_num()
				} else {
					self.prev_pic_info.frame_num_offset
				};

				let mut abs_frame_num = if sps.num_ref_frames_in_pic_order_cnt_cycle != 0 {
					pic.frame_num_offset + pic.frame_num
				} else {
					0
				};

				if pic.nal_ref_idc == 0 && abs_frame_num > 0 {
					abs_frame_num -= 1;
				}

				let mut expected_pic_order_cnt = 0;

				if abs_frame_num > 0 {
					if sps.num_ref_frames_in_pic_order_cnt_cycle == 0 {
						bail!("invalid num_ref_frames_in_pic_order_cnt_cycle");
					}

					let cycle_len = u32::from(sps.num_ref_frames_in_pic_order_cnt_cycle);
					let cycles = (abs_frame_num - 1) / cycle_len;
					// The cycle is only summed up to the position this frame sits
					// at within it, not all the way round.
					let position_in_cycle = (abs_frame_num - 1) % cycle_len;
					expected_pic_order_cnt = cycles as i32 * sps.expected_delta_per_pic_order_cnt_cycle;

					for i in 0..=position_in_cycle {
						expected_pic_order_cnt += sps.offset_for_ref_frame[i as usize];
					}
				}

				if pic.nal_ref_idc == 0 {
					expected_pic_order_cnt += sps.offset_for_non_ref_pic;
				}

				match pic.field {
					Field::Frame => {
						pic.top_field_order_cnt = expected_pic_order_cnt + pic.delta_pic_order_cnt0;
						pic.bottom_field_order_cnt =
							pic.top_field_order_cnt + sps.offset_for_top_to_bottom_field + pic.delta_pic_order_cnt1;
					}
					Field::Top => {
						pic.top_field_order_cnt = expected_pic_order_cnt + pic.delta_pic_order_cnt0;
					}
					Field::Bottom => {
						pic.bottom_field_order_cnt =
							expected_pic_order_cnt + sps.offset_for_top_to_bottom_field + pic.delta_pic_order_cnt0;
					}
				}
			}

			// 8.2.1.3
			2 => {
				if self.prev_pic_info.has_mmco_5 {
					self.prev_pic_info.frame_num_offset = 0;
				}

				pic.frame_num_offset = if matches!(pic.is_idr, IsIdr::Yes { .. }) {
					0
				} else if self.prev_pic_info.frame_num > pic.frame_num {
					self.prev_pic_info.frame_num_offset + sps.max_frame_num()
				} else {
					self.prev_pic_info.frame_num_offset
				};

				let pic_order_cnt = if matches!(pic.is_idr, IsIdr::Yes { .. }) {
					0
				} else if pic.nal_ref_idc == 0 {
					2 * (pic.frame_num_offset + pic.frame_num) as i32 - 1
				} else {
					2 * (pic.frame_num_offset + pic.frame_num) as i32
				};

				if matches!(pic.field, Field::Frame | Field::Top) {
					pic.top_field_order_cnt = pic_order_cnt;
				}
				if matches!(pic.field, Field::Frame | Field::Bottom) {
					pic.bottom_field_order_cnt = pic_order_cnt;
				}
			}

			other => bail!("invalid pic_order_cnt_type {other}"),
		}

		pic.pic_order_cnt = match pic.field {
			Field::Frame => std::cmp::min(pic.top_field_order_cnt, pic.bottom_field_order_cnt),
			Field::Top => pic.top_field_order_cnt,
			Field::Bottom => pic.bottom_field_order_cnt,
		};

		Ok(())
	}

	/// 8.2.5.1: marks the decoded picture as used or unused for reference.
	fn reference_pic_marking(&mut self, pic: &mut PictureData, sps: &Sps) -> anyhow::Result<()> {
		if matches!(pic.is_idr, IsIdr::Yes { .. }) {
			self.dpb.mark_all_as_unused_for_ref();

			if pic.ref_pic_marking.long_term_reference_flag {
				pic.set_reference(Reference::LongTerm, false);
				pic.long_term_frame_idx = 0;
				self.max_long_term_frame_idx = MaxLongTermFrameIdx::Idx(0);
			} else {
				pic.set_reference(Reference::ShortTerm, false);
				self.max_long_term_frame_idx = MaxLongTermFrameIdx::NoLongTermFrameIndices;
			}

			return Ok(());
		}

		if pic.ref_pic_marking.adaptive_ref_pic_marking_mode_flag {
			self.handle_memory_management_ops(pic)?;
		} else {
			self.dpb.sliding_window_marking(pic, sps);
		}

		Ok(())
	}

	/// 8.2.5.4: runs the picture's adaptive memory management control operations.
	fn handle_memory_management_ops(&mut self, pic: &mut PictureData) -> Result<(), MmcoError> {
		let markings = pic.ref_pic_marking.clone();

		for marking in &markings.inner {
			match marking.memory_management_control_operation {
				0 => break,
				1 => self.dpb.mmco_op_1(pic, marking)?,
				2 => self.dpb.mmco_op_2(pic, marking)?,
				3 => self.dpb.mmco_op_3(pic, marking)?,
				4 => self.max_long_term_frame_idx = self.dpb.mmco_op_4(marking),
				5 => self.max_long_term_frame_idx = self.dpb.mmco_op_5(pic),
				6 => self.dpb.mmco_op_6(pic, marking),
				other => return Err(MmcoError::UnknownMmco(other)),
			}
		}

		Ok(())
	}

	/// 8.2.4: derives RefPicList0 and RefPicList1 for one slice, from the
	/// per-picture lists the DPB built.
	fn create_ref_pic_lists<'a>(
		&'a self,
		pic: &PictureData,
		hdr: &SliceHeader,
		lists: &ReferencePicLists,
	) -> anyhow::Result<(DpbPicRefList<'a, Handle>, DpbPicRefList<'a, Handle>)> {
		let list0 = match hdr.slice_type {
			SliceType::P | SliceType::Sp => self.modify_ref_pic_list(pic, hdr, false, &lists.ref_pic_list_p0)?,
			SliceType::B => self.modify_ref_pic_list(pic, hdr, false, &lists.ref_pic_list_b0)?,
			_ => Vec::new(),
		};

		let list1 = match hdr.slice_type {
			SliceType::B => self.modify_ref_pic_list(pic, hdr, true, &lists.ref_pic_list_b1)?,
			_ => Vec::new(),
		};

		Ok((list0, list1))
	}

	/// 8.2.4.3: applies the slice header's reference picture list modifications.
	fn modify_ref_pic_list<'a>(
		&'a self,
		pic: &PictureData,
		hdr: &SliceHeader,
		list1: bool,
		indices: &[usize],
	) -> anyhow::Result<DpbPicRefList<'a, Handle>> {
		let (modify, num_ref_idx_active_minus1, modifications) = if list1 {
			(
				hdr.ref_pic_list_modification_flag_l1,
				hdr.num_ref_idx_l1_active_minus1,
				&hdr.ref_pic_list_modification_l1,
			)
		} else {
			(
				hdr.ref_pic_list_modification_flag_l0,
				hdr.num_ref_idx_l0_active_minus1,
				&hdr.ref_pic_list_modification_l0,
			)
		};

		let mut list: DpbPicRefList<'a, Handle> = indices
			.iter()
			.map(|&i| &self.dpb.entries()[i])
			.take(usize::from(num_ref_idx_active_minus1) + 1)
			.collect();

		if !modify {
			return Ok(list);
		}

		let mut pic_num_pred = pic.pic_num;
		let mut ref_idx = 0;

		for modification in modifications {
			match modification.modification_of_pic_nums_idc {
				idc @ (0 | 1) => self.short_term_modification(
					pic,
					&mut list,
					num_ref_idx_active_minus1,
					hdr.max_pic_num as i32,
					idc,
					modification,
					&mut pic_num_pred,
					&mut ref_idx,
				)?,
				2 => self.long_term_modification(&mut list, num_ref_idx_active_minus1, modification, &mut ref_idx)?,
				3 => break,
				other => bail!("unexpected modification_of_pic_nums_idc {other}"),
			}
		}

		Ok(list)
	}

	/// 8.2.4.3.1: modification for short-term reference pictures.
	fn short_term_modification<'a>(
		&'a self,
		pic: &PictureData,
		list: &mut DpbPicRefList<'a, Handle>,
		num_ref_idx_active_minus1: u8,
		max_pic_num: i32,
		idc: u8,
		modification: &RefPicListModification,
		pic_num_pred: &mut i32,
		ref_idx: &mut usize,
	) -> anyhow::Result<()> {
		let abs_diff_pic_num = modification.abs_diff_pic_num_minus1 as i32 + 1;

		let pic_num_no_wrap = if idc == 0 {
			if *pic_num_pred - abs_diff_pic_num < 0 {
				*pic_num_pred - abs_diff_pic_num + max_pic_num
			} else {
				*pic_num_pred - abs_diff_pic_num
			}
		} else if *pic_num_pred + abs_diff_pic_num >= max_pic_num {
			*pic_num_pred + abs_diff_pic_num - max_pic_num
		} else {
			*pic_num_pred + abs_diff_pic_num
		};

		*pic_num_pred = pic_num_no_wrap;

		let pic_num = if pic_num_no_wrap > pic.pic_num {
			pic_num_no_wrap - max_pic_num
		} else {
			pic_num_no_wrap
		};

		let entry = self
			.dpb
			.find_short_term_with_pic_num(pic_num)
			.with_context(|| format!("no short-term reference with pic_num {pic_num}"))?;

		Self::insert_modification(list, entry, num_ref_idx_active_minus1, ref_idx, |target| {
			target.pic_num_f(max_pic_num) != pic_num
		});

		Ok(())
	}

	/// 8.2.4.3.2: modification for long-term reference pictures.
	fn long_term_modification<'a>(
		&'a self,
		list: &mut DpbPicRefList<'a, Handle>,
		num_ref_idx_active_minus1: u8,
		modification: &RefPicListModification,
		ref_idx: &mut usize,
	) -> anyhow::Result<()> {
		let long_term_pic_num = modification.long_term_pic_num;
		let max_long_term_frame_idx = self.max_long_term_frame_idx;

		let entry = self
			.dpb
			.find_long_term_with_long_term_pic_num(long_term_pic_num)
			.with_context(|| format!("no long-term reference with long_term_pic_num {long_term_pic_num}"))?;

		Self::insert_modification(list, entry, num_ref_idx_active_minus1, ref_idx, |target| {
			target.long_term_pic_num_f(max_long_term_frame_idx) != long_term_pic_num
		});

		Ok(())
	}

	/// The shared tail of 8.2.4.3.1 and 8.2.4.3.2: insert `entry` at `ref_idx`,
	/// then shuffle out the duplicate `keep` identifies and re-truncate the list.
	fn insert_modification<'a>(
		list: &mut DpbPicRefList<'a, Handle>,
		entry: &'a DpbEntry<Handle>,
		num_ref_idx_active_minus1: u8,
		ref_idx: &mut usize,
		keep: impl Fn(&PictureData) -> bool,
	) {
		// A modification past the end of the list is a stream error; the spec
		// only defines indices within num_ref_idx_lX_active_minus1 + 1.
		let at = (*ref_idx).min(list.len());
		list.insert(at, entry);
		*ref_idx = at + 1;

		let mut next = *ref_idx;
		for current in *ref_idx..=usize::from(num_ref_idx_active_minus1) + 1 {
			if current >= list.len() {
				break;
			}

			if keep(&list[current].pic.borrow()) {
				list[next] = list[current];
				next += 1;
			}
		}

		list.truncate(usize::from(num_ref_idx_active_minus1) + 1);
	}
}

/// The picture currently being assembled, held across the slices that make it up.
struct CurrentPicture {
	pic: PictureData,
	/// PPS active for the last slice seen.
	pps: Rc<Pps>,
	/// Handle to the surface the driver renders into.
	handle: Handle,
	picture: Picture<crate::PictureNew, Handle>,
	/// Per-picture reference lists, modified per slice.
	ref_pic_lists: ReferencePicLists,
}

/// A decoded picture: the surface holding it plus what the caller needs to read
/// it back. Cloned into the DPB, so the surface is not recycled while it is still
/// a reference or still waiting to be output.
#[derive(Clone)]
struct Handle {
	surface: Arc<PooledSurface>,
	timestamp: u64,
	width: u32,
	height: u32,
}

impl std::borrow::Borrow<Surface<()>> for Handle {
	fn borrow(&self) -> &Surface<()> {
		self.surface.get()
	}
}

/// The properties of an SPS that the VA config, context, and surface pool are
/// built from. A change in any of them forces them to be rebuilt.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SequenceInfo {
	visible: (u32, u32),
	coded: (u32, u32),
	profile_idc: u8,
	bit_depth_luma_minus8: u8,
	bit_depth_chroma_minus8: u8,
	chroma_format_idc: u8,
	frame_mbs_only_flag: bool,
	max_dpb_frames: usize,
}

impl SequenceInfo {
	fn new(sps: &Sps) -> anyhow::Result<Self> {
		if !sps.frame_mbs_only_flag {
			bail!("interlaced H.264 is not supported");
		}
		if sps.chroma_format_idc != 1 {
			bail!(
				"only 4:2:0 chroma is supported, got chroma_format_idc {}",
				sps.chroma_format_idc
			);
		}
		if sps.bit_depth_luma_minus8 != 0 || sps.bit_depth_chroma_minus8 != 0 {
			bail!(
				"only 8-bit samples are supported, got {}-bit luma and {}-bit chroma",
				sps.bit_depth_luma_minus8 + 8,
				sps.bit_depth_chroma_minus8 + 8
			);
		}

		if sps.frame_cropping_flag && (sps.frame_crop_left_offset != 0 || sps.frame_crop_top_offset != 0) {
			bail!("left and top H.264 cropping is not supported");
		}
		let (crop_right, crop_bottom) = if sps.frame_cropping_flag {
			(sps.frame_crop_right_offset, sps.frame_crop_bottom_offset)
		} else {
			(0, 0)
		};
		let width = crop_right
			.checked_mul(2)
			.and_then(|crop| sps.width().checked_sub(crop))
			.context("invalid horizontal H.264 crop")?;
		let height = crop_bottom
			.checked_mul(2)
			.and_then(|crop| sps.height().checked_sub(crop))
			.context("invalid vertical H.264 crop")?;
		if width == 0 || height == 0 || width % 2 != 0 || height % 2 != 0 {
			bail!("visible size {width}x{height} is not a non-zero even 4:2:0 size");
		}

		Ok(Self {
			visible: (width, height),
			coded: (sps.width(), sps.height()),
			profile_idc: sps.profile_idc,
			bit_depth_luma_minus8: sps.bit_depth_luma_minus8,
			bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8,
			chroma_format_idc: sps.chroma_format_idc,
			frame_mbs_only_flag: sps.frame_mbs_only_flag,
			max_dpb_frames: sps.max_dpb_frames(),
		})
	}
}

/// What the VA config, context, and surface pool of a sequence are built for.
/// A change in any of it rebuilds them.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Format {
	codec: Codec,
	profile: VAProfile::Type,
	coded: (u32, u32),
	/// The size after cropping, which is what a [`Frame`] reports.
	visible: (u32, u32),
	/// The most pictures the DPB holds, which bounds what the pool keeps free.
	max_dpb_frames: usize,
}

/// The VA state for one coded video sequence.
struct Sequence {
	format: Format,
	pool: SurfacePool,
	// Drop order: the context must go before the config it was created from.
	context: Rc<Context>,
	_config: VaConfig,
}

impl Sequence {
	fn new(display: &Arc<Display>, format: Format) -> anyhow::Result<Self> {
		let attrs = vec![VAConfigAttrib {
			type_: VAConfigAttribType::VAConfigAttribRTFormat,
			value: VA_RT_FORMAT_YUV420,
		}];
		let config = display
			.create_config(attrs, format.profile, VAEntrypoint::VAEntrypointVLD)
			.map_err(|e| anyhow!("create VA config: {e:?}"))?;

		let (coded_width, coded_height) = format.coded;
		let context = display
			.create_context::<()>(&config, coded_width, coded_height, None, true)
			.map_err(|e| anyhow!("create VA context: {e:?}"))?;

		let (width, height) = format.visible;
		tracing::info!(
			"VA-API {:?} sequence: {width}x{height} visible, {coded_width}x{coded_height} coded, \
			 VA profile {}, DPB {} frames",
			format.codec,
			format.profile,
			format.max_dpb_frames
		);

		// What the decoder itself can need at once: a full DPB plus the picture
		// being decoded. Anything free beyond that was let go by a consumer that
		// had been holding pictures, and is memory the stream will not ask for
		// again.
		let capacity = format.max_dpb_frames + 1;
		Ok(Self {
			pool: SurfacePool::new(
				display,
				(coded_width, coded_height),
				UsageHint::USAGE_HINT_DECODER,
				capacity,
			),
			format,
			context,
			_config: config,
		})
	}

	/// Returns a surface to decode a picture coded at `timestamp` into.
	fn handle(&self, timestamp: u64) -> anyhow::Result<Handle> {
		let (width, height) = self.format.visible;
		Ok(Handle {
			surface: Arc::new(self.pool.alloc()?),
			timestamp,
			width,
			height,
		})
	}
}

/// Submits a picture with all its buffers added, and waits for it to decode.
///
/// The wait comes before the surface enters the DPB: it is about to be read as
/// a reference by later pictures, and the caller may never ask for its pixels.
/// Dropping the synced picture frees the VA buffers with it.
fn run(picture: Picture<crate::PictureNew, Handle>) -> anyhow::Result<()> {
	let picture = picture.begin::<()>().map_err(|e| anyhow!("picture begin: {e:?}"))?;
	let picture = picture.render().map_err(|e| anyhow!("picture render: {e:?}"))?;
	let picture = picture.end().map_err(|e| anyhow!("picture end: {e:?}"))?;
	drop(picture.sync::<()>().map_err(|(e, _)| anyhow!("picture sync: {e:?}"))?);
	Ok(())
}

/// Returns how many H.264 frames may wait for output before the one with the
/// lowest order count is due.
///
/// The VUI's `max_num_reorder_frames` when the SPS carries it, else the
/// inference E.2.1 makes from the profile, which is the DPB size for most
/// streams. An order count derived from `frame_num` (8.2.1.3) makes output
/// order decoding order, so such a stream reorders nothing whatever its VUI
/// says. A limit past the DPB size is out of range, and the DPB size bounds it.
fn reorder_limit(sps: &Sps) -> usize {
	if sps.pic_order_cnt_type == 2 {
		return 0;
	}
	(sps.max_num_order_frames() as usize).min(sps.max_dpb_frames())
}

/// Exports a decoded surface as a DRM PRIME descriptor, copying nothing.
fn export(handle: &Handle) -> anyhow::Result<ExportedFrame> {
	ExportedFrame::export(
		Arc::clone(&handle.surface),
		handle.timestamp,
		(handle.width, handle.height),
	)
}

/// Reads a decoded surface back to client memory as tightly packed NV12.
fn download(handle: &Handle) -> anyhow::Result<Frame> {
	read_back(handle.surface.get(), handle.timestamp, handle.width, handle.height)
}

/// Reads `surface` back to client memory as tightly packed NV12, cropped to
/// `width` by `height`.
///
/// The one read-back path, shared by the picture a decoder downloads itself and
/// the one a caller took as a descriptor and later wants on the CPU after all.
fn read_back(surface: &Surface<()>, timestamp: u64, width: u32, height: u32) -> anyhow::Result<Frame> {
	surface.sync().map_err(|e| anyhow!("surface sync: {e:?}"))?;

	let visible = (width, height);

	// vaDeriveImage exposes the surface's own buffer, so the copy below is the
	// only one. Not every driver can derive every surface; fall back to
	// vaCreateImage + vaGetImage, which always works but copies inside the driver
	// first.
	match Image::derive_from(surface, visible) {
		Ok(image) if image.image().format.fourcc == VA_FOURCC_NV12 => {
			return pack_nv12(&image, timestamp, width, height);
		}
		Ok(_) => tracing::debug!("derived image is not NV12, falling back to vaGetImage"),
		Err(e) => tracing::debug!("vaDeriveImage failed ({e:?}), falling back to vaGetImage"),
	}

	let format = surface
		.display()
		.query_image_formats()
		.map_err(|e| anyhow!("query image formats: {e:?}"))?
		.into_iter()
		.find(|format| format.fourcc == VA_FOURCC_NV12)
		.context("driver has no NV12 image format")?;
	let image = Image::create_from(surface, format, surface.size(), visible)
		.map_err(|e| anyhow!("vaGetImage into an NV12 image: {e:?}"))?;

	pack_nv12(&image, timestamp, width, height)
}

/// Copies a mapped NV12 image into a tightly packed buffer, dropping the row
/// padding the driver's pitch may carry.
fn pack_nv12(image: &Image<'_>, timestamp: u64, frame_width: u32, frame_height: u32) -> anyhow::Result<Frame> {
	let va_image = image.image();
	let src: &[u8] = image.as_ref();
	let (width, height) = (frame_width as usize, frame_height as usize);
	let chroma_rows = height / 2;

	let mut data = vec![0u8; width * (height + chroma_rows)];
	let (luma, chroma) = data.split_at_mut(width * height);
	copy_plane(
		luma,
		src,
		va_image.offsets[0] as usize,
		va_image.pitches[0] as usize,
		width,
		height,
	)?;
	copy_plane(
		chroma,
		src,
		va_image.offsets[1] as usize,
		va_image.pitches[1] as usize,
		width,
		chroma_rows,
	)?;

	Ok(Frame {
		timestamp,
		width: frame_width,
		height: frame_height,
		data,
	})
}

/// Copies `rows` rows of `width` bytes out of a strided plane.
fn copy_plane(
	dst: &mut [u8],
	src: &[u8],
	offset: usize,
	pitch: usize,
	width: usize,
	rows: usize,
) -> anyhow::Result<()> {
	if rows == 0 {
		return Ok(());
	}
	let end = offset + pitch * (rows - 1) + width;
	if pitch < width || src.len() < end {
		bail!("VA image plane (offset {offset}, pitch {pitch}) does not hold {rows} rows of {width} bytes");
	}

	for row in 0..rows {
		let from = offset + row * pitch;
		dst[row * width..][..width].copy_from_slice(&src[from..from + width]);
	}

	Ok(())
}

/// Checks that `display`'s driver can decode `codec`, which is what [`Decoder::new`] needs of a render node.
///
/// # Errors
///
/// Fails unless the driver offers `VAEntrypointVLD` for a Constrained
/// Baseline, Main, or High profile (H.264) or for the Main profile (H.265),
/// or when it cannot be asked.
pub fn probe(display: &Display, codec: Codec) -> anyhow::Result<()> {
	let profiles = display
		.query_config_profiles()
		.map_err(|e| anyhow!("query VA profiles: {e:?}"))?;

	let supported = profiles
		.into_iter()
		.filter(|profile| match codec {
			Codec::H264 => matches!(
				*profile,
				VAProfile::VAProfileH264ConstrainedBaseline
					| VAProfile::VAProfileH264Main
					| VAProfile::VAProfileH264High
			),
			Codec::H265 => *profile == VAProfile::VAProfileHEVCMain,
		})
		.any(|profile| {
			display
				.query_config_entrypoints(profile)
				.is_ok_and(|entrypoints| entrypoints.contains(&VAEntrypoint::VAEntrypointVLD))
		});

	if !supported {
		bail!("driver exposes no {codec:?} decode entrypoint");
	}

	Ok(())
}

/// Maps the SPS profile onto the VA profile to open the config with.
fn va_profile(sps: &Sps) -> anyhow::Result<VAProfile::Type> {
	let profile = Profile::try_from(sps.profile_idc).map_err(|e| anyhow!("{e}"))?;

	Ok(match profile {
		// VA-API has no plain Baseline profile, and the picture parameter buffer
		// has no way to describe the FMO and ASO tools that are all that separate
		// it from Constrained Baseline (num_slice_groups_minus1 is pinned to 0
		// below), so both map here. A stream that really uses them decodes wrong
		// rather than not at all, which no encoder in this decade emits.
		Profile::Baseline => VAProfile::VAProfileH264ConstrainedBaseline,
		Profile::Main | Profile::Extended => VAProfile::VAProfileH264Main,
		// The bit depth and chroma format are checked separately, so a High10 or
		// High422 SPS only reaches here when its samples are 8-bit 4:2:0.
		Profile::High | Profile::High10 | Profile::High422P => VAProfile::VAProfileH264High,
	})
}

fn create_buffer(context: &Rc<Context>, buffer: BufferType) -> anyhow::Result<Buffer> {
	context
		.create_buffer(buffer)
		.map_err(|e| anyhow!("create VA buffer: {e:?}"))
}

/// Cached variables from the previous reference picture, needed by 8.2.1.
#[derive(Debug)]
struct PrevReferencePicInfo {
	frame_num: u32,
	has_mmco_5: bool,
	top_field_order_cnt: i32,
	pic_order_cnt_msb: i32,
	pic_order_cnt_lsb: i32,
	field: Field,
}

impl Default for PrevReferencePicInfo {
	fn default() -> Self {
		Self {
			frame_num: 0,
			has_mmco_5: false,
			top_field_order_cnt: 0,
			pic_order_cnt_msb: 0,
			pic_order_cnt_lsb: 0,
			field: Field::Frame,
		}
	}
}

impl PrevReferencePicInfo {
	fn fill(&mut self, pic: &PictureData) {
		self.has_mmco_5 = pic.has_mmco_5;
		self.top_field_order_cnt = pic.top_field_order_cnt;
		self.pic_order_cnt_msb = pic.pic_order_cnt_msb;
		self.pic_order_cnt_lsb = pic.pic_order_cnt_lsb;
		self.field = pic.field;
		self.frame_num = pic.frame_num;
	}
}

/// Cached variables from the previous picture, needed by 8.2.1.
#[derive(Debug, Default)]
struct PrevPicInfo {
	frame_num: u32,
	frame_num_offset: u32,
	has_mmco_5: bool,
}

impl PrevPicInfo {
	fn fill(&mut self, pic: &PictureData) {
		self.frame_num = pic.frame_num;
		self.has_mmco_5 = pic.has_mmco_5;
		self.frame_num_offset = pic.frame_num_offset;
	}
}

/// The surface a DPB entry decoded into, or `VA_INVALID_ID` for the non-existing
/// pictures invented for a frame_num gap.
fn va_surface_id(handle: &Option<Handle>) -> bindings::VASurfaceID {
	match handle {
		Some(handle) => handle.surface.id(),
		None => VA_INVALID_ID,
	}
}

/// Ported from cros-codecs `fill_va_h264_pic` (BSD-3-Clause). Progressive only,
/// so the field cases collapse to the frame one.
fn fill_va_h264_pic(pic: &PictureData, surface_id: bindings::VASurfaceID) -> PictureH264 {
	let mut flags = 0;
	let frame_idx = match pic.reference() {
		Reference::LongTerm => {
			flags |= VA_PICTURE_H264_LONG_TERM_REFERENCE;
			pic.long_term_frame_idx
		}
		Reference::ShortTerm => {
			flags |= VA_PICTURE_H264_SHORT_TERM_REFERENCE;
			pic.frame_num
		}
		Reference::None => pic.frame_num,
	};

	PictureH264::new(
		surface_id,
		frame_idx,
		flags,
		pic.top_field_order_cnt,
		pic.bottom_field_order_cnt,
	)
}

/// Builds the picture used to fill the array slots there is no reference for.
fn build_invalid_va_h264_pic() -> PictureH264 {
	PictureH264::new(VA_INVALID_ID, 0, VA_PICTURE_H264_INVALID, 0, 0)
}

/// Ported from cros-codecs `build_iq_matrix` (BSD-3-Clause). VA-API wants the
/// scaling lists in raster order, the PPS carries them zig-zag.
fn build_iq_matrix(pps: &Pps) -> BufferType {
	const ZIGZAG_4X4: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
	#[rustfmt::skip]
	const ZIGZAG_8X8: [usize; 64] = [
		0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
		20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51,
		58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
	];

	let mut scaling_list4x4 = [[0u8; 16]; 6];
	for (list, src) in scaling_list4x4.iter_mut().zip(pps.scaling_lists_4x4.iter()) {
		for (i, &value) in src.iter().enumerate() {
			list[ZIGZAG_4X4[i]] = value;
		}
	}

	let mut scaling_list8x8 = [[0u8; 64]; 2];
	for (list, src) in scaling_list8x8.iter_mut().zip(pps.scaling_lists_8x8.iter()) {
		for (i, &value) in src.iter().enumerate() {
			list[ZIGZAG_8X8[i]] = value;
		}
	}

	BufferType::IQMatrix(IQMatrix::H264(IQMatrixBufferH264::new(
		scaling_list4x4,
		scaling_list8x8,
	)))
}

/// Ported from cros-codecs `build_pic_param` (BSD-3-Clause).
fn build_pic_param(
	hdr: &SliceHeader,
	pic: &PictureData,
	surface_id: bindings::VASurfaceID,
	dpb: &Dpb<Handle>,
	sps: &Sps,
	pps: &Pps,
) -> BufferType {
	let curr_pic = fill_va_h264_pic(pic, surface_id);

	let short_term = dpb
		.short_term_refs_iter()
		.filter(|entry| !entry.pic.borrow().nonexisting);
	let mut refs: Vec<PictureH264> = short_term
		.chain(dpb.long_term_refs_iter())
		.take(16)
		.map(|entry| fill_va_h264_pic(&entry.pic.borrow(), va_surface_id(&entry.reference)))
		.collect();
	refs.resize_with(16, build_invalid_va_h264_pic);
	let refs: [PictureH264; 16] = refs.try_into().unwrap_or_else(|_| unreachable!("resized to 16"));

	let seq_fields = H264SeqFields::new(
		sps.chroma_format_idc as u32,
		sps.separate_colour_plane_flag as u32,
		sps.gaps_in_frame_num_value_allowed_flag as u32,
		sps.frame_mbs_only_flag as u32,
		sps.mb_adaptive_frame_field_flag as u32,
		sps.direct_8x8_inference_flag as u32,
		// See A.3.3.2.
		(sps.level_idc >= Level::L3_1) as u32,
		sps.log2_max_frame_num_minus4 as u32,
		sps.pic_order_cnt_type as u32,
		sps.log2_max_pic_order_cnt_lsb_minus4 as u32,
		sps.delta_pic_order_always_zero_flag as u32,
	);

	let pic_fields = H264PicFields::new(
		pps.entropy_coding_mode_flag as u32,
		pps.weighted_pred_flag as u32,
		pps.weighted_bipred_idc as u32,
		pps.transform_8x8_mode_flag as u32,
		hdr.field_pic_flag as u32,
		pps.constrained_intra_pred_flag as u32,
		pps.bottom_field_pic_order_in_frame_present_flag as u32,
		pps.deblocking_filter_control_present_flag as u32,
		pps.redundant_pic_cnt_present_flag as u32,
		(pic.nal_ref_idc != 0) as u32,
	);

	// SequenceInfo rejects interlaced input, so map units are frame macroblocks.
	let picture_height_in_mbs_minus1 = sps.pic_height_in_map_units_minus1;

	BufferType::PictureParameter(PictureParameter::H264(PictureParameterBufferH264::new(
		curr_pic,
		refs,
		sps.pic_width_in_mbs_minus1,
		picture_height_in_mbs_minus1,
		sps.bit_depth_luma_minus8,
		sps.bit_depth_chroma_minus8,
		sps.max_num_ref_frames,
		&seq_fields,
		// FMO is not expressible in VA-API.
		0,
		0,
		0,
		pps.pic_init_qp_minus26,
		pps.pic_init_qs_minus26,
		pps.chroma_qp_index_offset,
		pps.second_chroma_qp_index_offset,
		&pic_fields,
		hdr.frame_num,
	)))
}

/// Ported from cros-codecs `fill_ref_pic_list` (BSD-3-Clause).
fn fill_ref_pic_list(list: &[&DpbEntry<Handle>]) -> [PictureH264; 32] {
	let mut pics: Vec<PictureH264> = list
		.iter()
		.take(32)
		.map(|entry| fill_va_h264_pic(&entry.pic.borrow(), va_surface_id(&entry.reference)))
		.collect();
	pics.resize_with(32, build_invalid_va_h264_pic);

	pics.try_into().unwrap_or_else(|_| unreachable!("resized to 32"))
}

/// Ported from cros-codecs `build_slice_param` (BSD-3-Clause).
fn build_slice_param(
	hdr: &SliceHeader,
	slice_size: usize,
	list0: &[&DpbEntry<Handle>],
	list1: &[&DpbEntry<Handle>],
	sps: &Sps,
	pps: &Pps,
) -> BufferType {
	let pwt = &hdr.pred_weight_table;

	let mut luma_weight_l0 = [0i16; 32];
	let mut luma_offset_l0 = [0i16; 32];
	let mut chroma_weight_l0 = [[0i16; 2]; 32];
	let mut chroma_offset_l0 = [[0i16; 2]; 32];

	let mut luma_weight_l1 = [0i16; 32];
	let mut luma_offset_l1 = [0i16; 32];
	let mut chroma_weight_l1 = [[0i16; 2]; 32];
	let mut chroma_offset_l1 = [[0i16; 2]; 32];

	// Explicit weighted prediction: P and SP slices weight list 0 only, B slices
	// weight both. Implicit (weighted_bipred_idc 2) is derived by the hardware.
	let fill_l0 = (pps.weighted_pred_flag && (hdr.slice_type.is_p() || hdr.slice_type.is_sp()))
		|| (pps.weighted_bipred_idc == 1 && hdr.slice_type.is_b());
	let fill_l1 = pps.weighted_bipred_idc == 1 && hdr.slice_type.is_b();
	let chroma = sps.chroma_array_type() != 0;

	if fill_l0 {
		for i in 0..=usize::from(hdr.num_ref_idx_l0_active_minus1) {
			luma_weight_l0[i] = pwt.luma_weight_l0[i];
			luma_offset_l0[i] = i16::from(pwt.luma_offset_l0[i]);
			if chroma {
				for j in 0..2 {
					chroma_weight_l0[i][j] = pwt.chroma_weight_l0[i][j];
					chroma_offset_l0[i][j] = i16::from(pwt.chroma_offset_l0[i][j]);
				}
			}
		}
	}

	if fill_l1 {
		for i in 0..=usize::from(hdr.num_ref_idx_l1_active_minus1) {
			luma_weight_l1[i] = pwt.luma_weight_l1[i];
			luma_offset_l1[i] = i16::from(pwt.luma_offset_l1[i]);
			if chroma {
				for j in 0..2 {
					chroma_weight_l1[i][j] = pwt.chroma_weight_l1[i][j];
					chroma_offset_l1[i][j] = i16::from(pwt.chroma_offset_l1[i][j]);
				}
			}
		}
	}

	BufferType::SliceParameter(SliceParameter::H264(SliceParameterBufferH264::new(
		slice_size as u32,
		0,
		VA_SLICE_DATA_FLAG_ALL,
		hdr.header_bit_size as u16,
		hdr.first_mb_in_slice as u16,
		hdr.slice_type as u8,
		hdr.direct_spatial_mv_pred_flag as u8,
		hdr.num_ref_idx_l0_active_minus1,
		hdr.num_ref_idx_l1_active_minus1,
		hdr.cabac_init_idc,
		hdr.slice_qp_delta,
		hdr.disable_deblocking_filter_idc,
		hdr.slice_alpha_c0_offset_div2,
		hdr.slice_beta_offset_div2,
		fill_ref_pic_list(list0),
		fill_ref_pic_list(list1),
		pwt.luma_log2_weight_denom,
		pwt.chroma_log2_weight_denom,
		fill_l0 as u8,
		luma_weight_l0,
		luma_offset_l0,
		(fill_l0 && chroma) as u8,
		chroma_weight_l0,
		chroma_offset_l0,
		fill_l1 as u8,
		luma_weight_l1,
		luma_offset_l1,
		(fill_l1 && chroma) as u8,
		chroma_weight_l1,
		chroma_offset_l1,
	)))
}

#[cfg(test)]
mod tests {
	use std::os::fd::OwnedFd;
	use std::process::Command;

	use super::*;

	fn progressive_sps() -> Sps {
		Sps {
			frame_mbs_only_flag: true,
			chroma_format_idc: 1,
			pic_width_in_mbs_minus1: 19,
			pic_height_in_map_units_minus1: 14,
			..Default::default()
		}
	}

	#[test]
	fn rejects_nonzero_crop_origins() {
		for (left, top) in [(1, 0), (0, 1)] {
			let sps = Sps {
				frame_cropping_flag: true,
				frame_crop_left_offset: left,
				frame_crop_top_offset: top,
				..progressive_sps()
			};
			assert!(SequenceInfo::new(&sps)
				.unwrap_err()
				.to_string()
				.contains("left and top"));
		}
	}

	#[test]
	fn crop_only_updates_change_sequence_identity() {
		let original = SequenceInfo::new(&progressive_sps()).unwrap();
		for (right, bottom, expected) in [(1, 0, (318, 240)), (0, 1, (320, 238))] {
			let cropped = SequenceInfo::new(&Sps {
				frame_cropping_flag: true,
				frame_crop_right_offset: right,
				frame_crop_bottom_offset: bottom,
				..progressive_sps()
			})
			.unwrap();
			assert_eq!(original.coded, cropped.coded);
			assert_eq!(cropped.visible, expected);
			assert_ne!(original, cropped);
		}
	}

	#[test]
	fn rejects_invalid_crop_dimensions() {
		for crop in [160, 161, u32::MAX] {
			assert!(SequenceInfo::new(&Sps {
				frame_cropping_flag: true,
				frame_crop_right_offset: crop,
				..progressive_sps()
			})
			.is_err());
		}
	}

	#[test]
	fn maximum_progressive_height_does_not_overflow_va_parameters() {
		let sps = Rc::new(Sps {
			pic_height_in_map_units_minus1: u16::MAX,
			..progressive_sps()
		});
		let params = build_pic_param(
			&SliceHeader::default(),
			&PictureData::new_non_existing(0, 0),
			VA_INVALID_ID,
			&Dpb::default(),
			&sps,
			&crate::codec::h264::parser::PpsBuilder::new(Rc::clone(&sps)).build(),
		);
		let BufferType::PictureParameter(PictureParameter::H264(params)) = params else {
			panic!("expected H.264 picture parameters");
		};
		assert_eq!(params.inner().picture_height_in_mbs_minus1, u16::MAX);
		assert!(SequenceInfo::new(&Sps {
			frame_mbs_only_flag: false,
			..progressive_sps()
		})
		.is_err());
	}

	/// Encoder settings shared by the tests that need a stream rather than a
	/// particular picture.
	fn encoder_config(width: u32, height: u32) -> crate::encode::Config {
		crate::encode::Config::new(width, height, 30, 2_000_000, 30)
	}

	/// A gray field with a moving block, so a picture that decoded into the
	/// wrong surface is visible as the block being in the wrong place.
	fn nv12_frame(width: u32, height: u32, step: u32) -> Vec<u8> {
		let (w, h) = (width as usize, height as usize);
		let mut data = vec![128u8; w * h * 3 / 2];
		let x0 = (step * 16) as usize % (w / 2);
		for y in h / 4..h / 2 {
			for x in x0..x0 + w / 4 {
				data[y * w + x] = 235;
			}
		}
		data
	}

	/// Exported pictures the caller still holds never share a surface.
	///
	/// The invariant `export` exists to keep: a held surface stays out of the
	/// pool, because recycling it would have a later picture decoded over pixels
	/// the caller still holds a descriptor for. Two pictures coming
	/// back with the same modifier and layout is expected; two coming back on
	/// the same allocation is the bug. The modifier itself is hardware policy,
	/// so it is printed rather than asserted.
	#[test]
	fn exported_pictures_do_not_share_a_surface() {
		let (width, height) = (320u32, 240u32);
		let Ok(mut encoder) = crate::encode::Encoder::new(encoder_config(width, height)) else {
			eprintln!("skipping: no VA-API H.264 encoder");
			return;
		};
		let Ok(mut decoder) = Decoder::new(Config::new()) else {
			eprintln!("skipping: no VA-API H.264 decoder");
			return;
		};

		let mut exported = Vec::new();
		for step in 0..8 {
			let unit = encoder
				.encode_nv12(&nv12_frame(width, height, step), step == 0)
				.expect("encode a picture");
			exported.extend(decoder.decode_exported(&unit, step as u64).expect("decode a picture"));
		}
		exported.extend(decoder.flush_exported().expect("flush the decoder"));
		assert!(!exported.is_empty(), "the decoder produced no pictures");

		let first = &exported[0];
		eprintln!(
			"exported {} pictures: {}x{} fourcc {:#x} modifier {:#x}",
			exported.len(),
			first.width,
			first.height,
			first.descriptor.fourcc,
			first.descriptor.objects[0].drm_format_modifier
		);

		let mut seen = Vec::new();
		for (index, frame) in exported.iter().enumerate() {
			assert_eq!((frame.width, frame.height), (width, height));
			assert_eq!(frame.descriptor.fourcc, VA_FOURCC_NV12);
			assert_eq!(frame.descriptor.objects.len(), 1, "a composed export is one object");
			assert_eq!(frame.descriptor.layers[0].num_planes, 2, "NV12 is two planes");

			// Two live descriptors for one allocation would mean the pool handed
			// a held surface back out. The inode behind a DMA-BUF names the
			// buffer, and is what identifies one allocation from another.
			let inode = inode(&frame.descriptor.objects[0].fd);
			assert!(
				!seen.contains(&inode),
				"picture {index} was decoded into an allocation an earlier one still holds"
			);
			seen.push(inode);
		}
	}

	/// With no node named, the decoder opens one that decodes H.264. A named
	/// node is used or refused, never swapped for another.
	#[test]
	fn the_render_node_is_found_or_named() {
		if let Err(err) = Decoder::new(Config::new()) {
			eprintln!("skipping: no VA-API H.264 decoder: {err:#}");
			return;
		}
		let named = Config {
			device: Some(PathBuf::from("/dev/null")),
			..Config::new()
		};
		assert!(
			Decoder::new(named).is_err(),
			"a named node that is not a render node was swapped for one that is"
		);
	}

	/// Every picture fed in comes back out, the tail only on flush.
	///
	/// A stream with B-frames holds pictures in the DPB until later ones come
	/// out ahead of them, so a stream that simply stops leaves its tail there.
	/// The first assertion keeps the test honest, since a decoder that held
	/// nothing back would satisfy the rest without a flush ever running.
	#[test]
	fn flush_returns_the_pictures_the_dpb_still_holds() {
		const PICTURES: usize = 10;

		let Some(stream) = software_encode(Codec::H264, (320, 240), PICTURES, true) else {
			eprintln!("skipping: no ffmpeg that encodes H.264");
			return;
		};
		let Ok(mut decoder) = Decoder::new(Config::new()) else {
			eprintln!("skipping: no VA-API H.264 decoder");
			return;
		};

		let mut streamed = Vec::new();
		for (index, unit) in split_access_units(Codec::H264, &stream).iter().enumerate() {
			streamed.extend(decoder.decode(unit, index as u64).expect("decode a picture"));
		}
		assert!(
			streamed.len() < PICTURES,
			"the DPB held nothing back, so this test proves nothing"
		);

		let flushed = decoder.flush().expect("flush the decoder");
		let mut timestamps: Vec<u64> = streamed
			.iter()
			.chain(&flushed)
			.map(|picture| picture.timestamp)
			.collect();
		timestamps.sort_unstable();
		assert_eq!(
			timestamps,
			(0..PICTURES as u64).collect::<Vec<_>>(),
			"the stream lost pictures at its end"
		);

		// The bumping process marks what it hands out, so a second flush finds
		// nothing and a caller that flushes twice never sees a picture twice.
		assert!(decoder.flush().expect("flush an empty decoder").is_empty());
	}

	/// An H.264 stream coded without reordering comes out as it decodes, from
	/// this crate's encoder and from libx264.
	///
	/// Both declare one frame buffer, which C.4.5.3 would empty only when the
	/// next picture needs it, so each picture used to come out one access unit
	/// late. The two cover both ways a stream rules reordering out: this
	/// crate's encoder writes `max_num_reorder_frames` 0 in its VUI, and libx264
	/// without B-frames derives the order count from `frame_num` (type 2).
	#[test]
	fn h264_without_reordering_comes_out_as_it_decodes() {
		const PICTURES: usize = 10;
		let (width, height) = (320u32, 240u32);

		let mut streams = Vec::new();
		match crate::encode::Encoder::new(encoder_config(width, height)) {
			Ok(mut encoder) => {
				let units = (0..PICTURES)
					.map(|step| {
						encoder
							.encode_nv12(&nv12_frame(width, height, step as u32), step == 0)
							.expect("encode a picture")
					})
					.collect::<Vec<_>>();
				streams.push(("VA-API", units));
			}
			Err(_) => eprintln!("skipping this crate's encoder: no VA-API H.264 encoder"),
		}
		match software_encode(Codec::H264, (width, height), PICTURES, false) {
			Some(stream) => streams.push(("libx264", split_access_units(Codec::H264, &stream))),
			None => eprintln!("skipping libx264: no ffmpeg that encodes H.264"),
		}

		for (encoder, units) in streams {
			let Ok(mut decoder) = Decoder::new(Config::new()) else {
				eprintln!("skipping: no VA-API H.264 decoder");
				return;
			};
			for (index, unit) in units.iter().enumerate() {
				let decoded = decoder.decode(unit, index as u64).expect("decode an access unit");
				let timestamps: Vec<u64> = decoded.iter().map(|picture| picture.timestamp).collect();
				assert_eq!(
					timestamps,
					[index as u64],
					"{encoder}: access unit {index} was held back"
				);
			}
			assert!(decoder.flush().expect("flush the decoder").is_empty());
		}
	}

	/// The reorder limit comes from the VUI, is bounded by the DPB size, and is
	/// zero whenever the order count follows `frame_num`.
	#[test]
	fn the_reorder_limit_follows_the_sps() {
		let sps = || Sps {
			level_idc: Level::L3,
			max_num_ref_frames: 1,
			..progressive_sps()
		};
		// 8100 macroblocks over 300 per picture, capped at 16.
		assert_eq!(sps().max_dpb_frames(), 16);
		assert_eq!(
			reorder_limit(&sps()),
			16,
			"without a VUI, the inference is the DPB size"
		);

		let mut declared = Sps {
			vui_parameters_present_flag: true,
			..sps()
		};
		declared.vui_parameters.bitstream_restriction_flag = true;
		declared.vui_parameters.max_num_reorder_frames = 0;
		declared.vui_parameters.max_dec_frame_buffering = 1;
		assert_eq!(reorder_limit(&declared), 0, "the VUI's limit applies");

		declared.vui_parameters.max_num_reorder_frames = 5;
		declared.vui_parameters.max_dec_frame_buffering = 2;
		assert_eq!(reorder_limit(&declared), 2, "the DPB size bounds the VUI's limit");

		let derived = Sps {
			pic_order_cnt_type: 2,
			..sps()
		};
		assert_eq!(
			reorder_limit(&derived),
			0,
			"an order count from frame_num reorders nothing"
		);
	}

	/// A picture the consumer has let go of is decoded into again rather than
	/// replaced by a new allocation, and the recycled surfaces still carry the
	/// right pictures.
	///
	/// Each picture is read back and dropped before the next access unit goes
	/// in, the way a renderer that draws a picture and moves on behaves. Without
	/// recycling this allocated one surface per picture.
	#[test]
	fn a_dropped_export_is_decoded_into_again() {
		const PICTURES: u32 = 60;
		let (width, height) = (320u32, 240u32);
		let Ok(mut encoder) = crate::encode::Encoder::new(encoder_config(width, height)) else {
			eprintln!("skipping: no VA-API H.264 encoder");
			return;
		};
		let Ok(mut exporting) = Decoder::new(Config::new()) else {
			eprintln!("skipping: no VA-API H.264 decoder");
			return;
		};
		let mut downloading = Decoder::new(Config::new()).expect("a second decoder");

		let mut compared = 0;
		for step in 0..PICTURES {
			let unit = encoder
				.encode_nv12(&nv12_frame(width, height, step), step == 0)
				.expect("encode a picture");
			let exported = exporting
				.decode_exported(&unit, step as u64)
				.expect("decode to a descriptor");
			let downloaded = downloading.decode(&unit, step as u64).expect("decode to client memory");
			assert_eq!(exported.len(), downloaded.len(), "the two decoders disagreed");
			for (gpu, cpu) in exported.iter().zip(&downloaded) {
				let read_back = gpu.download().expect("read the exported surface back");
				assert!(read_back.data == cpu.data, "picture {} came back wrong", gpu.timestamp);
				compared += 1;
			}
		}
		assert!(compared > 0, "the decoder produced no pictures");

		let pool = &exporting.sequence().expect("a sequence").pool;
		let allocated = pool.allocated.get();
		eprintln!("{allocated} surfaces allocated for {PICTURES} exported pictures");
		assert!(
			allocated <= pool.free.capacity,
			"{allocated} surfaces for {PICTURES} pictures; a full DPB and the picture in flight need {}",
			pool.free.capacity
		);
	}

	/// A consumer that holds many pictures and then lets them all go leaves at
	/// most the pool's capacity behind, and the next pictures reuse those.
	#[test]
	fn the_pool_keeps_a_bounded_number_of_free_surfaces() {
		const HELD: u32 = 40;
		let (width, height) = (320u32, 240u32);
		let Ok(mut encoder) = crate::encode::Encoder::new(encoder_config(width, height)) else {
			eprintln!("skipping: no VA-API H.264 encoder");
			return;
		};
		let Ok(mut decoder) = Decoder::new(Config::new()) else {
			eprintln!("skipping: no VA-API H.264 decoder");
			return;
		};

		let mut step = 0;
		let mut decode = |decoder: &mut Decoder| {
			let unit = encoder
				.encode_nv12(&nv12_frame(width, height, step), step == 0)
				.expect("encode a picture");
			step += 1;
			decoder.decode_exported(&unit, step as u64).expect("decode a picture")
		};

		let mut held = Vec::new();
		for _ in 0..HELD {
			held.extend(decode(&mut decoder));
		}
		let pool = &decoder.sequence().expect("a sequence").pool;
		let allocated = pool.allocated.get();
		assert!(
			allocated >= held.len(),
			"{} pictures held on {allocated} surfaces",
			held.len()
		);

		drop(held);
		let free = pool.free.surfaces.lock().unwrap().len();
		assert!(
			free <= pool.free.capacity,
			"{free} surfaces left free, more than the capacity of {}",
			pool.free.capacity
		);

		for _ in 0..10 {
			drop(decode(&mut decoder));
		}
		let pool = &decoder.sequence().expect("a sequence").pool;
		assert_eq!(
			pool.allocated.get(),
			allocated,
			"pictures after the release did not reuse the free surfaces"
		);
	}

	/// An exported picture crosses threads, which is the point of handing one to
	/// a renderer: the decoder runs on its own thread and the frame outlives it.
	/// [`Decoder`] is neither [`Send`] nor [`Sync`], so this pins that the export
	/// does not inherit that.
	#[test]
	fn an_exported_frame_is_send_and_sync() {
		const fn assert_send_sync<T: Send + Sync>() {}
		assert_send_sync::<ExportedFrame>();
	}

	/// An exported picture reads back to the same pixels the ordinary path
	/// downloads.
	///
	/// The invariant that lets a GPU-resident frame keep a CPU fallback: the
	/// surface travels with the descriptor, so `vaDeriveImage` still
	/// reaches the picture the descriptor points at. One stream through two
	/// decoders, so both sides see the same pictures in the same order and the
	/// comparison is exact rather than approximate.
	#[test]
	fn an_exported_picture_downloads_to_the_same_pixels() {
		let (width, height) = (320u32, 240u32);
		let Ok(mut encoder) = crate::encode::Encoder::new(encoder_config(width, height)) else {
			eprintln!("skipping: no VA-API H.264 encoder");
			return;
		};
		let Ok(mut exporting) = Decoder::new(Config::new()) else {
			eprintln!("skipping: no VA-API H.264 decoder");
			return;
		};
		let mut downloading = Decoder::new(Config::new()).expect("a second decoder");

		let mut exported = Vec::new();
		let mut downloaded = Vec::new();
		for step in 0..8 {
			let unit = encoder
				.encode_nv12(&nv12_frame(width, height, step), step == 0)
				.expect("encode a picture");
			exported.extend(
				exporting
					.decode_exported(&unit, step as u64)
					.expect("decode to a descriptor"),
			);
			downloaded.extend(downloading.decode(&unit, step as u64).expect("decode to client memory"));
		}
		exported.extend(exporting.flush_exported().expect("flush the exporting decoder"));
		downloaded.extend(downloading.flush().expect("flush the downloading decoder"));

		assert!(!exported.is_empty(), "the decoder produced no pictures");
		assert_eq!(exported.len(), downloaded.len(), "the two decoders disagreed");

		for (index, (gpu, cpu)) in exported.iter().zip(&downloaded).enumerate() {
			let read_back = gpu.download().expect("read the exported surface back");
			assert_eq!(read_back.timestamp, cpu.timestamp, "picture {index} lost its timestamp");
			assert_eq!((read_back.width, read_back.height), (cpu.width, cpu.height));
			assert!(
				read_back.data == cpu.data,
				"picture {index} read back differently than it downloaded"
			);
		}
	}

	/// Every picture matches what ffmpeg's software decoder makes of the same
	/// stream, bit for bit, in either codec.
	///
	/// The test that says this is a decoder rather than plumbing. The streams
	/// are coded with B-frames and three reference frames, so matching the
	/// reference means the reference lists, the picture order counts, and the
	/// DPB bumping are all right, not just the surfaces. The second size crops
	/// on the right and bottom. Half the pictures are taken as descriptors and
	/// downloaded after, so both ways out are checked. Skips without ffmpeg,
	/// its encoder, or a device.
	#[test]
	fn decoded_pictures_match_a_software_decoder() {
		const PICTURES: usize = 30;

		for codec in [Codec::H264, Codec::H265] {
			for (width, height) in [(320u32, 240u32), (852, 478)] {
				let Some(stream) = software_encode(codec, (width, height), PICTURES, true) else {
					eprintln!("skipping: no ffmpeg that encodes {codec:?}");
					continue;
				};
				let reference = software_decode(codec, &stream);

				let picture_len = (width * height * 3 / 2) as usize;
				assert_eq!(
					reference.len(),
					PICTURES * picture_len,
					"ffmpeg decoded a different count"
				);

				let Ok(mut decoder) = Decoder::new(Config { codec, ..Config::new() }) else {
					eprintln!("skipping: no VA-API {codec:?} decoder");
					continue;
				};

				let units = split_access_units(codec, &stream);
				let mut decoded = Vec::new();
				for (index, unit) in units.iter().enumerate() {
					if index % 2 == 0 {
						decoded.extend(decoder.decode(unit, index as u64).expect("decode an access unit"));
					} else {
						for exported in decoder
							.decode_exported(unit, index as u64)
							.expect("decode an access unit")
						{
							decoded.push(exported.download().expect("download an exported picture"));
						}
					}
				}
				decoded.extend(decoder.flush().expect("flush the decoder"));
				assert_eq!(decoded.len(), PICTURES, "{codec:?} lost or invented pictures");

				let mut timestamps: Vec<u64> = decoded.iter().map(|picture| picture.timestamp).collect();
				assert!(
					timestamps.windows(2).any(|pair| pair[0] > pair[1]),
					"the {codec:?} stream came out in decode order, so nothing here tests reordering"
				);
				timestamps.sort_unstable();
				assert_eq!(
					timestamps,
					(0..PICTURES as u64).collect::<Vec<_>>(),
					"a {codec:?} picture's timestamp did not survive reordering"
				);

				for (index, picture) in decoded.iter().enumerate() {
					assert_eq!((picture.width, picture.height), (width, height));
					assert!(
						picture.data == reference[index * picture_len..][..picture_len],
						"{codec:?} picture {index} of {PICTURES} at {width}x{height} differs from the software decoder"
					);
				}
			}
		}
	}

	/// An H.265 stream coded without reordering comes out as it decodes: C.5.2
	/// bumps by the SPS's reorder limit, which is zero, rather than holding
	/// pictures until the DPB fills the way H.264's C.4.5.3 does.
	#[test]
	fn h265_without_reordering_comes_out_as_it_decodes() {
		const PICTURES: usize = 10;

		let Some(stream) = software_encode(Codec::H265, (320, 240), PICTURES, false) else {
			eprintln!("skipping: no ffmpeg that encodes H.265");
			return;
		};
		let Ok(mut decoder) = Decoder::new(Config {
			codec: Codec::H265,
			..Config::new()
		}) else {
			eprintln!("skipping: no VA-API H.265 decoder");
			return;
		};

		for (index, unit) in split_access_units(Codec::H265, &stream).iter().enumerate() {
			let decoded = decoder.decode(unit, index as u64).expect("decode an access unit");
			let timestamps: Vec<u64> = decoded.iter().map(|picture| picture.timestamp).collect();
			assert_eq!(timestamps, [index as u64], "access unit {index} was held back");
		}
		assert!(decoder.flush().expect("flush the decoder").is_empty());
	}

	/// A 4K H.265 stream coded with wavefronts, as x265 codes by default, signals
	/// an entry point per CTB row: 33 at 64-pixel CTBs, past what a fixed array
	/// of 32 held.
	#[test]
	fn a_4k_h265_wavefront_stream_decodes() {
		const PICTURES: usize = 2;
		let size = (3840, 2160);

		let Some(stream) = software_encode(Codec::H265, size, PICTURES, false) else {
			eprintln!("skipping: no ffmpeg that encodes H.265");
			return;
		};
		let Ok(mut decoder) = Decoder::new(Config {
			codec: Codec::H265,
			..Config::new()
		}) else {
			eprintln!("skipping: no VA-API H.265 decoder");
			return;
		};

		let mut decoded = Vec::new();
		for (index, unit) in split_access_units(Codec::H265, &stream).iter().enumerate() {
			decoded.extend(decoder.decode(unit, index as u64).expect("decode an access unit"));
		}
		decoded.extend(decoder.flush().expect("flush the decoder"));
		assert_eq!(decoded.len(), PICTURES);
		assert!(decoded.iter().all(|picture| (picture.width, picture.height) == size));
	}

	/// A second H.265 sequence at another size decodes at that size, and the
	/// first sequence's tail comes out ahead of it.
	#[test]
	fn a_new_h265_sequence_decodes_at_its_own_size() {
		const PER_SEQUENCE: usize = 6;
		let sizes = [(320u32, 240u32), (192u32, 144u32)];

		let mut stream = Vec::new();
		for &size in &sizes {
			let Some(sequence) = software_encode(Codec::H265, size, PER_SEQUENCE, true) else {
				eprintln!("skipping: no ffmpeg that encodes H.265");
				return;
			};
			stream.extend(sequence);
		}
		let Ok(mut decoder) = Decoder::new(Config {
			codec: Codec::H265,
			..Config::new()
		}) else {
			eprintln!("skipping: no VA-API H.265 decoder");
			return;
		};

		let mut decoded = Vec::new();
		let units = split_access_units(Codec::H265, &stream);
		for (index, unit) in units.iter().enumerate() {
			decoded.extend(decoder.decode(unit, index as u64).expect("decode an access unit"));
		}
		decoded.extend(decoder.flush().expect("flush the decoder"));

		let expected: Vec<(u32, u32)> = sizes
			.iter()
			.flat_map(|&size| std::iter::repeat_n(size, PER_SEQUENCE))
			.collect();
		let actual: Vec<(u32, u32)> = decoded.iter().map(|picture| (picture.width, picture.height)).collect();
		assert_eq!(actual, expected, "a picture came out at the wrong size or out of order");

		for picture in &decoded {
			let expected_len = (picture.width * picture.height * 3 / 2) as usize;
			assert_eq!(
				picture.data.len(),
				expected_len,
				"a picture was read back at the wrong size"
			);
		}
	}

	/// A second sequence at another size decodes at that size, and the first
	/// sequence's tail comes out ahead of it.
	///
	/// What `apply_sps` promises: an SPS that changes the coded size rebuilds the
	/// VA context and the surface pool, and everything decoded against the old
	/// one is let out first so no picture is read back at the wrong size.
	#[test]
	fn a_new_sequence_decodes_at_its_own_size() {
		const PER_SEQUENCE: u32 = 6;
		let sizes = [(320u32, 240u32), (192u32, 144u32)];

		let Ok(mut decoder) = Decoder::new(Config::new()) else {
			eprintln!("skipping: no VA-API H.264 decoder");
			return;
		};

		let mut decoded = Vec::new();
		let mut timestamp = 0u64;
		for &(width, height) in &sizes {
			let Ok(mut encoder) = crate::encode::Encoder::new(encoder_config(width, height)) else {
				eprintln!("skipping: no VA-API H.264 encoder");
				return;
			};
			for step in 0..PER_SEQUENCE {
				let unit = encoder
					.encode_nv12(&nv12_frame(width, height, step), step == 0)
					.expect("encode a picture");
				decoded.extend(decoder.decode(&unit, timestamp).expect("decode a picture"));
				timestamp += 1;
			}
		}
		decoded.extend(decoder.flush().expect("flush the decoder"));

		let expected: Vec<(u32, u32)> = sizes
			.iter()
			.flat_map(|&size| std::iter::repeat_n(size, PER_SEQUENCE as usize))
			.collect();
		let actual: Vec<(u32, u32)> = decoded.iter().map(|picture| (picture.width, picture.height)).collect();
		assert_eq!(actual, expected, "a picture came out at the wrong size or out of order");

		let timestamps: Vec<u64> = decoded.iter().map(|picture| picture.timestamp).collect();
		assert_eq!(timestamps, (0..timestamp).collect::<Vec<_>>());

		for picture in &decoded {
			let expected_len = (picture.width * picture.height * 3 / 2) as usize;
			assert_eq!(
				picture.data.len(),
				expected_len,
				"a picture was read back at the wrong size"
			);
		}
	}

	/// Runs ffmpeg with `args`, returning its standard output, or `None` when
	/// ffmpeg is missing or refused the arguments.
	fn ffmpeg(args: &[&str]) -> Option<Vec<u8>> {
		let output = Command::new("ffmpeg").arg("-v").arg("error").args(args).output().ok()?;
		if !output.status.success() {
			eprintln!("ffmpeg {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
			return None;
		}
		Some(output.stdout)
	}

	/// Encodes `pictures` of ffmpeg's test pattern at `size`, with two B-frames
	/// and three reference frames when `reorder` is set and neither otherwise,
	/// returning the Annex-B stream. `None` without ffmpeg or its encoder.
	fn software_encode(codec: Codec, (width, height): (u32, u32), pictures: usize, reorder: bool) -> Option<Vec<u8>> {
		let source = format!("testsrc2=size={width}x{height}:rate=30");
		let pictures = pictures.to_string();
		let mut args = vec![
			"-f",
			"lavfi",
			"-i",
			&source,
			"-frames:v",
			&pictures,
			"-pix_fmt",
			"yuv420p",
		];

		let x265;
		match codec {
			Codec::H264 => args.extend([
				"-c:v",
				"libx264",
				"-profile:v",
				"main",
				"-bf",
				if reorder { "2" } else { "0" },
				"-refs",
				if reorder { "3" } else { "1" },
				"-g",
				"15",
				"-f",
				"h264",
			]),
			Codec::H265 => {
				// x265's defaults are open GOPs, so the second keyframe is a CRA
				// picture with leading pictures that refer across it.
				x265 = if reorder {
					"bframes=2:ref=3:keyint=15:min-keyint=15:log-level=error"
				} else {
					"bframes=0:ref=1:keyint=15:min-keyint=15:log-level=error"
				};
				args.extend(["-c:v", "libx265", "-x265-params", x265, "-f", "hevc"]);
			}
		}
		args.push("-");
		ffmpeg(&args)
	}

	/// Decodes `stream` with ffmpeg into packed NV12, one picture after another.
	fn software_decode(codec: Codec, stream: &[u8]) -> Vec<u8> {
		// ffmpeg reads the elementary stream from a file rather than a pipe, so
		// the test never has to drain its output while still writing its input.
		static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
		let count = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		let extension = match codec {
			Codec::H264 => "264",
			Codec::H265 => "265",
		};
		let path = std::env::temp_dir().join(format!("moq-vaapi-decode-{}-{count}.{extension}", std::process::id()));
		std::fs::write(&path, stream).expect("write the elementary stream");
		let decoded = ffmpeg(&[
			"-i",
			path.to_str().expect("a UTF-8 temporary path"),
			"-pix_fmt",
			"nv12",
			"-f",
			"rawvideo",
			"-",
		]);
		let _ = std::fs::remove_file(&path);
		decoded.expect("ffmpeg decodes what it just encoded")
	}

	/// Splits an Annex-B elementary stream into access units.
	///
	/// A unit ends at the first slice of the next picture, or at a parameter
	/// set, SEI, or delimiter that comes before one.
	fn split_access_units(codec: Codec, stream: &[u8]) -> Vec<Vec<u8>> {
		let starts: Vec<usize> = (0..stream.len().saturating_sub(3))
			.filter(|&i| stream[i..i + 3] == [0, 0, 1])
			.collect();

		let mut units = Vec::new();
		let mut current: Vec<u8> = Vec::new();
		let mut current_has_picture = false;

		for (index, &start) in starts.iter().enumerate() {
			let end = starts.get(index + 1).copied().unwrap_or(stream.len());
			let nal = &stream[start + 3..end];
			// The first bit after the NAL header is first_mb_in_slice == 0 for
			// H.264 and first_slice_segment_in_pic_flag for H.265: either way, set
			// on the first slice of a picture.
			let (is_slice, starts_picture, starts_unit) = match codec {
				Codec::H264 => {
					let nal_type = nal[0] & 0x1f;
					let is_slice = (1..=5).contains(&nal_type);
					(is_slice, is_slice && nal[1] & 0x80 != 0, matches!(nal_type, 6..=9))
				}
				Codec::H265 => {
					let nal_type = (nal[0] >> 1) & 0x3f;
					let is_slice = nal_type <= 31;
					(
						is_slice,
						is_slice && nal[2] & 0x80 != 0,
						matches!(nal_type, 32..=35 | 39),
					)
				}
			};

			if current_has_picture && (starts_picture || starts_unit) {
				units.push(std::mem::take(&mut current));
				current_has_picture = false;
			}

			current.extend_from_slice(&stream[start..end]);
			current_has_picture |= is_slice;
		}

		if !current.is_empty() {
			units.push(current);
		}
		units
	}

	/// The inode of a descriptor, which for a DMA-BUF names the allocation
	/// behind it: every buffer gets its own on the anonymous dma-buf filesystem.
	fn inode(fd: &OwnedFd) -> u64 {
		use std::os::unix::fs::MetadataExt as _;

		// Stat a duplicate, so the descriptor the caller holds is untouched and
		// the `File` wrapper closes only its own copy.
		let file = std::fs::File::from(fd.try_clone().expect("duplicate the descriptor"));
		file.metadata().expect("stat the descriptor").ino()
	}
}
