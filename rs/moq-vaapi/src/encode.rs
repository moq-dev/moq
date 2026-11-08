// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
// Adapted from discord/cros-codecs (encoder/stateless/h264/vaapi.rs), itself
// BSD-3-Clause / Copyright The ChromiumOS Authors. See LICENSE.cros-codecs.

//! Thin VA-API H.264 and H.265 encoder built on the vendored libva binding.
//!
//! For H.264 it reuses the backend-agnostic bitstream layer
//! ([`crate::codec::h264`]) from discord/cros-codecs (BSD-3-Clause) for
//! SPS/PPS/slice synthesis, and drives libva directly for surface upload +
//! slice submission, rather than vendoring cros-codecs's generic multi-backend
//! encoder framework. The per-frame VA buffer population is ported from
//! cros-codecs's `encoder/stateless/h264/vaapi.rs`. H.265 writes its own
//! parameter sets and slice headers (see `h265.rs`).
//!
//! Low-latency only: IPPP (no B-frames), one reference frame, matching the
//! VideoToolbox / Media Foundation / NVENC backends in moq-video.
//!
//! Three ways in, cheapest first:
//! - [`Encoder::encode_dmabuf`] with an NV12 DMA-BUF at the encoder's size
//!   encodes the producer's memory in place.
//! - [`Encoder::encode_dmabuf`] with any other importable DMA-BUF (packed RGB
//!   from a screen capture, YUYV from a camera, NV12 at another size) converts
//!   and scales it into the encoder's own surface with the video processor, on
//!   the GPU.
//! - [`Encoder::encode_nv12`] uploads CPU pixels.
//!
//! Checked on Intel Meteor Lake and Arrow Lake with the iHD driver, and on an
//! AMD RX 9070 with radeonsi, by decoding with ffmpeg's software decoder: CPU
//! and DMA-BUF input come back above 35 dB luma PSNR, and BGRX converts to
//! BT.601 and BT.709 with every channel within 6 code values of the reference.
//! The low-power entrypoint is untested, since those devices expose the full
//! one.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use crate::codec::h264::parser::SpsBuilder;
use crate::codec::h264::parser::{Level, Pps, PpsBuilder, Profile, SliceHeader, SliceHeaderBuilder, SliceType, Sps};
use crate::codec::h264::synthesizer::Synthesizer;
use crate::codec::Codec;
use crate::dmabuf::DmaBuf;
use crate::vpp::Processor;
use crate::{
	BufferType, Color, Config as VaConfig, Context, Display, EncCodedBuffer, EncMiscParameter,
	EncMiscParameterFrameRate, EncMiscParameterRateControl, EncPackedHeaderParameter, EncPackedHeaderType,
	EncPictureParameter, EncPictureParameterBufferH264, EncSequenceParameter, EncSequenceParameterBufferH264,
	EncSliceParameter, EncSliceParameterBufferH264, H264EncFrameCropOffsets, H264EncPicFields, H264EncSeqFields,
	H264VuiFields, Image, MappedCodedBuffer, Picture, PictureH264, RcFlags, Surface, SurfaceMemoryDescriptor,
	UsageHint, VAConfigAttrib, VAConfigAttribType, VAEntrypoint, VAProfile, VA_ATTRIB_NOT_SUPPORTED,
	VA_ENC_PACKED_HEADER_PICTURE, VA_ENC_PACKED_HEADER_SEQUENCE, VA_ENC_PACKED_HEADER_SLICE, VA_FOURCC_ARGB,
	VA_FOURCC_BGRA, VA_FOURCC_BGRX, VA_FOURCC_NV12, VA_FOURCC_RGBA, VA_FOURCC_RGBX, VA_FOURCC_XRGB, VA_INVALID_ID,
	VA_PICTURE_H264_INVALID, VA_PICTURE_H264_SHORT_TERM_REFERENCE, VA_RC_CBR, VA_RT_FORMAT_YUV420,
};

mod h265;

/// Whether a frame is used as a reference, and for how long.
/// (Vendored from discord/cros-codecs, BSD-3-Clause; used by the slice synthesizer.)
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum IsReference {
	No,
	ShortTerm,
	LongTerm,
}

/// Quantizer bounds for the rate controller.
///
/// The floor keeps an IDR on easy content from taking a large share of the
/// rate. The ceiling is the codecs' own: iHD and radeonsi both honor it, so a
/// lower one sets a smallest picture, and a stream asked for less overshoots
/// its rate several times over. A lower ceiling does steady iHD's H.264 IDRs,
/// but under congestion missing the rate costs more than that.
const MIN_QP: u8 = 18;
const MAX_QP: u8 = 51;
/// Window over which libva's rate controller targets the configured bitrate.
///
/// This matches cros-codecs. The old `framerate * 1000` value confused frames
/// per second with seconds and gave a 30 fps encoder a 30-second window.
pub const RATE_CONTROL_WINDOW_MS: u32 = 1_500;
/// `max_frame_num` upper bound for the low-delay GOP; matches cros-codecs.
const LIMIT: u32 = 2048;

/// Encoder configuration. Dimensions must be even (4:2:0 chroma).
#[derive(Clone, Debug)]
pub struct Config {
	/// The format to encode, H.264 unless set. Main profile either way.
	pub codec: Codec,
	pub width: u32,
	pub height: u32,
	pub framerate: u32,
	/// Target bitrate in bits per second (CBR).
	pub bitrate: u32,
	/// Keyframe interval in frames.
	pub gop: u32,
	/// DRM render node to open (e.g. `/dev/dri/renderD128`).
	///
	/// `None`, the default, opens the first node whose driver can encode
	/// [`codec`](Self::codec), and [`Encoder::config`] then names the node it
	/// picked.
	pub device: Option<PathBuf>,
	/// Color space written into the SPS, and the target of a conversion from RGB.
	///
	/// Defaults to [`Color::infer`] for the height, which is what a decoder
	/// assumes for an unlabelled stream.
	pub color: Color,
}

impl Config {
	/// Returns an H.264 configuration; set [`codec`](Self::codec) for another format.
	pub fn new(width: u32, height: u32, framerate: u32, bitrate: u32, gop: u32) -> Self {
		Self {
			codec: Codec::H264,
			width,
			height,
			framerate,
			bitrate,
			gop,
			device: None,
			color: Color::infer(height),
		}
	}
}

/// Per-frame reference metadata. (Mirrors cros-codecs `DpbEntryMeta`.)
#[derive(Copy, Clone)]
struct FrameMeta {
	poc: u32,
	frame_num: u32,
}

impl FrameMeta {
	/// Returns the H.264 order count and frame number of the picture `count` pictures after an IDR.
	///
	/// frame_num counts modulo MaxFrameNum, and the slice header carries only
	/// the low bits of the picture order count; the full count goes to the
	/// driver in the picture parameters.
	fn new(count: u32) -> Self {
		Self {
			poc: count.wrapping_mul(2),
			frame_num: count % LIMIT,
		}
	}
}

/// The picture a P picture predicts from: the previous one.
#[derive(Copy, Clone)]
struct Reference {
	/// Its reconstructed surface.
	surface: crate::bindings::VASurfaceID,
	/// How many pictures after the IDR it came.
	count: u32,
}

/// What one picture's parameter buffers are built from.
struct PictureInfo<'a> {
	idr: bool,
	/// How many IDR pictures came before this one.
	idr_id: u32,
	/// How many pictures after the IDR this one comes.
	count: u32,
	/// The surface its reconstruction is written to.
	recon: crate::bindings::VASurfaceID,
	reference: Option<Reference>,
	coded: &'a EncCodedBuffer,
	bitrate: u32,
}

/// The bitstream syntax of the stream being encoded.
enum Syntax {
	H264 {
		sps: Rc<Sps>,
		pps: Rc<Pps>,
		/// The packed header kinds the driver takes from us; it writes the rest.
		packed: u32,
	},
	H265 {
		stream: h265::Stream,
		/// The packed header kinds the driver takes from us; it writes the rest.
		packed: u32,
	},
}

/// A VA-API H.264 or H.265 encoder. Built once, fed NV12 frames, emits an Annex-B stream.
pub struct Encoder {
	config: Config,
	syntax: Syntax,

	// Source surface (reused; NV12 re-uploaded per frame) and a 2-deep
	// reconstructed surface ring (current + previous reference for IPPP).
	input: Option<Surface<()>>,
	recon: Vec<Surface<()>>,
	/// Reference from the previous encoded frame: (recon index, pictures after the IDR).
	reference: Option<(usize, u32)>,

	counter: u32,
	/// IDR pictures coded so far, so two in a row carry different H.264 `idr_pic_id`s.
	idrs: u32,

	/// The video processor converting and scaling DMA-BUF input, opened on the
	/// first frame that needs it.
	processor: Option<Processor>,
	/// Set by a bitrate change, and cleared by the next frame, which asks the
	/// driver to restart its rate controller at the new target.
	reset_rate_control: bool,

	// Drop order: encode resources before the display they belong to.
	coded: EncCodedBuffer,
	coded_size: usize,
	context: Rc<Context>,
	_va_config: VaConfig,
	display: Arc<Display>,
}

impl Encoder {
	/// Opens [`Config::device`], or the first render node that encodes [`Config::codec`], and sets up an encode session for `config`.
	///
	/// # Errors
	///
	/// Fails when libva cannot be loaded, when the named node cannot be opened
	/// or does not encode the codec (a named node is never swapped for
	/// another), when no node encodes it, and when the driver refuses the
	/// session or, for H.265, requires a coding tool this encoder does not write
	/// or takes no packed sequence header.
	pub fn new(mut config: Config) -> anyhow::Result<Self> {
		let codec = config.codec;
		let (device, display) = match config.device.take() {
			Some(device) => {
				let display = Display::open_drm_display(&device)
					.map_err(|e| anyhow::anyhow!("open DRM display {device:?}: {e:?}"))?;
				(device, display)
			}
			None => crate::display::open_first(|display| probe(display, codec))
				.map_err(|e| e.context(format!("find a render node that encodes {codec:?}")))?,
		};

		let entrypoint = encode_entrypoint(&display, codec)?;
		let mut attrs = vec![
			VAConfigAttrib {
				type_: VAConfigAttribType::VAConfigAttribRTFormat,
				value: VA_RT_FORMAT_YUV420,
			},
			VAConfigAttrib {
				type_: VAConfigAttribType::VAConfigAttribRateControl,
				value: VA_RC_CBR,
			},
		];
		// The parameter sets go out as packed headers, so the VUI (color, frame
		// buffering) and the level are ours. For H.264 that is the SPS and PPS;
		// iHD takes them without being told, and a driver that follows the spec
		// writes its own unless the config asks. For H.265 it is the VPS, SPS and
		// PPS as one sequence header. Each slice header is packed too, as ffmpeg
		// packs them; the driver writes the slice headers if it does not take
		// them. radeonsi needs them for H.264: it takes the slice's NAL header
		// from the packed one, and drops the packed parameter sets without one.
		let wanted = match codec {
			Codec::H264 => VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_PICTURE | VA_ENC_PACKED_HEADER_SLICE,
			Codec::H265 => VA_ENC_PACKED_HEADER_SEQUENCE | VA_ENC_PACKED_HEADER_SLICE,
		};
		let packed = packed_headers(&display, codec, entrypoint) & wanted;
		// VA's H.265 sequence parameters have no conformance window and carry no
		// VUI here, so an SPS the driver writes would lose the crop and the color.
		if codec == Codec::H265 && packed & VA_ENC_PACKED_HEADER_SEQUENCE == 0 {
			anyhow::bail!("the driver takes no packed H.265 sequence header, which the crop and color need");
		}
		if packed != 0 {
			attrs.push(VAConfigAttrib {
				type_: VAConfigAttribType::VAConfigAttribEncPackedHeaders,
				value: packed,
			});
		} else {
			tracing::warn!("the driver takes no packed {codec:?} headers; the stream carries the driver's own");
		}
		let mut va_config = display
			.create_config(attrs, profile(codec), entrypoint)
			.map_err(|e| anyhow::anyhow!("create VA config: {e:?}"))?;

		let syntax = match codec {
			Codec::H264 => {
				let (sps, pps) = build_sps_pps(&config);
				Syntax::H264 { sps, pps, packed }
			}
			Codec::H265 => Syntax::H265 {
				stream: h265::Stream::new(&display, entrypoint, &mut va_config, &config)?,
				packed,
			},
		};
		// H.264 sizes the context and its references to the frame, which has
		// always worked; H.265 to whole minimum coding blocks, as ffmpeg does,
		// and to the driver's surface alignment.
		let size = match &syntax {
			Syntax::H264 { .. } => (config.width, config.height),
			Syntax::H265 { stream, .. } => stream.coded(),
		};

		let context = display
			.create_context::<()>(&va_config, size.0, size.1, None, true)
			.map_err(|e| anyhow::anyhow!("create VA context: {e:?}"))?;

		let coded_size = coded_size(config.bitrate);
		let coded = context
			.create_enc_coded(coded_size)
			.map_err(|e| anyhow::anyhow!("create coded buffer: {e:?}"))?;

		let input = make_surface(&display, (config.width, config.height))?;
		let recon = vec![make_surface(&display, size)?, make_surface(&display, size)?];

		tracing::info!(
			"opened VA-API {codec:?} encoder on {:?}: {}x{} @ {}fps, {} bps, low power: {}",
			device,
			config.width,
			config.height,
			config.framerate,
			config.bitrate,
			entrypoint == VAEntrypoint::VAEntrypointEncSliceLP,
		);
		config.device = Some(device);
		Ok(Self {
			config,
			syntax,
			input: Some(input),
			recon,
			reference: None,
			counter: 0,
			idrs: 0,
			processor: None,
			reset_rate_control: false,
			coded,
			coded_size,
			context,
			_va_config: va_config,
			display,
		})
	}

	/// Encodes one tightly-packed NV12 frame, returning Annex-B (with the
	/// parameter sets inline on each IDR). `keyframe` forces an IDR.
	///
	/// # Errors
	///
	/// Fails when `nv12` is shorter than a frame at the encoder's size, and when
	/// the upload or the encode fails.
	pub fn encode_nv12(&mut self, nv12: &[u8], keyframe: bool) -> anyhow::Result<Vec<u8>> {
		let (width, height) = (self.config.width as usize, self.config.height as usize);
		let expected = width * height + width * height.div_ceil(2);
		if nv12.len() < expected {
			anyhow::bail!("NV12 frame is {} bytes, {width}x{height} needs {expected}", nv12.len());
		}
		let input = self.input.take().expect("input surface present");
		let result = upload_nv12(&self.display, &input, self.config.width, self.config.height, nv12)
			.and_then(|()| self.encode_surface(&input, keyframe));
		self.input = Some(input);
		result
	}

	/// Encodes one frame held in a DMA-BUF, returning Annex-B like
	/// [`encode_nv12`](Self::encode_nv12).
	///
	/// An NV12 buffer at the encoder's size, which the encoder reads without
	/// padding (whole macroblocks for H.264; for H.265, whole minimum coding
	/// blocks at the driver's surface alignment), and whose
	/// [`color`](DmaBuf::color) is the stream's or unknown, is imported and
	/// encoded in place. Any other mapped format or size goes through the video processor into the encoder's own surface
	/// first: scaled to the encoder's size and converted to [`Config::color`].
	/// A YUV buffer with no color of its own is taken to be in that space
	/// already. Nothing reaches the CPU on either path.
	///
	/// The macroblock condition is not a formality: the encoder reads whole
	/// macroblocks, so it reads 1088 rows of a 1080-row picture, and a producer
	/// that places the chroma plane right after the last visible luma row
	/// (PipeWire does) has nothing there to read.
	///
	/// The producer must have finished writing the buffer; this does not wait
	/// on its fence.
	///
	/// # Errors
	///
	/// Fails when the driver refuses to import the buffer (an unsupported
	/// modifier or layout), when the device has no video processor and the
	/// buffer needs one, and when the encode itself fails. The encoder stays
	/// usable after a refused import, so a caller can fall back to
	/// [`encode_nv12`](Self::encode_nv12) for the same frame.
	pub fn encode_dmabuf(&mut self, buffer: DmaBuf, keyframe: bool) -> anyhow::Result<Vec<u8>> {
		let fourcc = buffer
			.fourcc()
			.ok_or_else(|| anyhow::anyhow!("no VA-API format for DRM format {:#010x}", buffer.drm_format))?;
		let color = self.config.color;
		let size = (self.config.width, self.config.height);
		let unpadded = match &self.syntax {
			Syntax::H264 { .. } => size.0 % 16 == 0 && size.1 % 16 == 0,
			Syntax::H265 { stream, .. } => stream.coded() == size,
		};
		let in_place = fourcc == VA_FOURCC_NV12
			&& (buffer.width, buffer.height) == size
			&& unpadded
			&& buffer.color.is_none_or(|own| own == color);
		if in_place {
			let source = buffer.import(&self.display, Some(UsageHint::USAGE_HINT_ENCODER))?;
			return self.encode_surface(&source, keyframe);
		}

		let input_color = match is_rgb(fourcc) {
			true => None,
			false => Some(buffer.color.unwrap_or(color)),
		};
		let source = buffer.import(&self.display, Some(UsageHint::USAGE_HINT_VPP_READ))?;
		let input = self.input.take().expect("input surface present");
		let result = self
			.processor()
			.and_then(|processor| processor.blit(&source, &input, input_color, Some(color)))
			.and_then(|()| self.encode_surface(&input, keyframe));
		self.input = Some(input);
		result
	}

	/// Changes the target bitrate, in bits per second, from the next frame on.
	///
	/// The rate control parameters go out with every frame, so this neither
	/// forces an IDR nor rebuilds the session; the next frame also asks the
	/// driver to reset its rate controller. It then converges over its window,
	/// [`RATE_CONTROL_WINDOW_MS`].
	///
	/// The level in the SPS is not revisited, so a rate far above the one the
	/// stream opened with can exceed what that level allows.
	///
	/// # Errors
	///
	/// Fails for a bitrate of zero, and when a larger coded buffer for the new
	/// rate cannot be allocated, in which case the old rate stays.
	pub fn set_bitrate(&mut self, bitrate: u32) -> anyhow::Result<()> {
		if bitrate == 0 {
			anyhow::bail!("a bitrate of zero");
		}
		let size = coded_size(bitrate);
		if size > self.coded_size {
			// A higher rate can produce a picture larger than the buffer sized for
			// the old one, and a truncated picture is corrupt rather than short.
			self.coded = self
				.context
				.create_enc_coded(size)
				.map_err(|e| anyhow::anyhow!("create coded buffer: {e:?}"))?;
			self.coded_size = size;
		}
		if bitrate != self.config.bitrate {
			self.reset_rate_control = true;
		}
		self.config.bitrate = bitrate;
		Ok(())
	}

	/// Returns the configuration in effect, including a bitrate changed since opening and the render node picked when none was named.
	pub fn config(&self) -> &Config {
		&self.config
	}

	/// Returns the video processor, opening it on first use.
	fn processor(&mut self) -> anyhow::Result<&Processor> {
		if self.processor.is_none() {
			self.processor = Some(Processor::with_display(Arc::clone(&self.display))?);
		}
		Ok(self.processor.as_ref().expect("opened above"))
	}

	/// Encodes the picture in `source`, which must be NV12 at the encoder's size.
	fn encode_surface<D: SurfaceMemoryDescriptor>(
		&mut self,
		source: &Surface<D>,
		keyframe: bool,
	) -> anyhow::Result<Vec<u8>> {
		let is_idr = keyframe || self.counter % self.config.gop == 0;
		if is_idr {
			// An IDR restarts the picture count and drops the reference.
			self.counter = 0;
			self.reference = None;
		}

		let recon_idx = (self.counter as usize) % self.recon.len();
		// The reference is the previous picture's reconstruction, for a P picture.
		let reference = match (is_idr, self.reference) {
			(false, Some((index, count))) => Some(Reference {
				surface: self.recon[index].id(),
				count,
			}),
			_ => None,
		};
		let info = PictureInfo {
			idr: is_idr,
			idr_id: self.idrs,
			count: self.counter,
			recon: self.recon[recon_idx].id(),
			reference,
			coded: &self.coded,
			bitrate: self.config.bitrate,
		};
		let buffers = match &self.syntax {
			Syntax::H264 { sps, pps, packed } => h264_buffers(sps, pps, &info, *packed)?,
			Syntax::H265 { stream, packed } => stream.buffers(&self.config, &info, *packed),
		};

		let mut picture = Picture::new(self.counter as u64, Rc::clone(&self.context), source);

		// VA-API spec buffer order: sequence, picture, slice, then packed headers,
		// then rate-control misc params.
		for buffer in buffers {
			picture.add_buffer(self.create(buffer)?);
		}

		let bits_per_second = self.config.bitrate;
		let rc = EncMiscParameterRateControl::new(
			bits_per_second,
			100,                    // target_percentage (CBR)
			RATE_CONTROL_WINDOW_MS, // window_size (ms)
			u32::from((MIN_QP + MAX_QP) / 2),
			MIN_QP as u32,
			0, // basic_unit_size
			// Do not let rate control drop a frame to meet the bitrate. A live
			// encoder has already admitted this frame into the media timeline.
			RcFlags::new(u32::from(self.reset_rate_control), 1, 0, 0, 0, 0, 0, 0, 0),
			0, // icq_quality_factor
			MAX_QP as u32,
			0, // quality_factor
			0, // target_frame_size
		);
		picture.add_buffer(self.create(BufferType::EncMiscParameter(EncMiscParameter::RateControl(rc)))?);

		let framerate = EncMiscParameterFrameRate::new(self.config.framerate, 0);
		picture.add_buffer(self.create(BufferType::EncMiscParameter(EncMiscParameter::FrameRate(framerate)))?);

		let picture = picture.begin().map_err(|e| anyhow::anyhow!("picture begin: {e:?}"))?;
		let picture = picture.render().map_err(|e| anyhow::anyhow!("picture render: {e:?}"))?;
		let picture = picture.end().map_err(|e| anyhow::anyhow!("picture end: {e:?}"))?;
		// Sync (PictureEnd -> PictureSync) so the source is no longer read and
		// the coded buffer is ready.
		picture
			.sync()
			.map_err(|(e, _)| anyhow::anyhow!("picture sync: {e:?}"))?;

		// The reconstructed surface syncs implicitly; read the coded bitstream.
		let bitstream = self.read_coded()?;

		self.reference = Some((recon_idx, self.counter));
		self.counter += 1;
		self.idrs = self.idrs.wrapping_add(u32::from(is_idr));
		self.reset_rate_control = false;
		Ok(bitstream)
	}

	fn create(&self, buffer: BufferType) -> anyhow::Result<crate::Buffer> {
		self.context
			.create_buffer(buffer)
			.map_err(|e| anyhow::anyhow!("create VA buffer: {e:?}"))
	}

	fn read_coded(&self) -> anyhow::Result<Vec<u8>> {
		let mapped = MappedCodedBuffer::new(&self.coded).map_err(|e| anyhow::anyhow!("map coded buffer: {e:?}"))?;
		let mut out = Vec::new();
		for segment in mapped.iter() {
			out.extend_from_slice(segment.buf);
		}
		Ok(out)
	}
}

/// Checks that `display`'s driver can encode `codec`, which is what [`Encoder::new`] needs of a render node.
///
/// # Errors
///
/// Fails when the driver offers neither the full nor the low-power Main
/// profile encode entrypoint for `codec`, or cannot be asked.
pub fn probe(display: &Display, codec: Codec) -> anyhow::Result<()> {
	encode_entrypoint(display, codec).map(drop)
}

/// Returns the VA profile `codec` is encoded in.
fn profile(codec: Codec) -> VAProfile::Type {
	match codec {
		Codec::H264 => VAProfile::VAProfileH264Main,
		Codec::H265 => h265::PROFILE,
	}
}

/// Returns the entrypoint to encode `codec` with: the full one, or the low-power one where it is all there is.
///
/// Some Intel parts expose encode only through `VAEntrypointEncSliceLP`.
fn encode_entrypoint(display: &Display, codec: Codec) -> anyhow::Result<VAEntrypoint::Type> {
	let entrypoints = display
		.query_config_entrypoints(profile(codec))
		.map_err(|e| anyhow::anyhow!("query {codec:?} Main entrypoints: {e:?}"))?;
	pick_entrypoint(&entrypoints).ok_or_else(|| anyhow::anyhow!("the device has no {codec:?} Main encode entrypoint"))
}

/// Returns the encode entrypoint to use out of those a device offers: the full one when present.
fn pick_entrypoint(offered: &[VAEntrypoint::Type]) -> Option<VAEntrypoint::Type> {
	[VAEntrypoint::VAEntrypointEncSlice, VAEntrypoint::VAEntrypointEncSliceLP]
		.into_iter()
		.find(|entrypoint| offered.contains(entrypoint))
}

/// Returns the packed header kinds the driver takes for `codec` at `entrypoint`.
///
/// Zero when it takes none, or when asking fails.
fn packed_headers(display: &Display, codec: Codec, entrypoint: VAEntrypoint::Type) -> u32 {
	let mut attrs = [VAConfigAttrib {
		type_: VAConfigAttribType::VAConfigAttribEncPackedHeaders,
		value: 0,
	}];
	if display
		.get_config_attributes(profile(codec), entrypoint, &mut attrs)
		.is_err()
		|| attrs[0].value & VA_ATTRIB_NOT_SUPPORTED != 0
	{
		return 0;
	}
	attrs[0].value
}

/// Returns the lowest H.264 level, from 4 up, whose frame size and macroblock rate cover the stream.
///
/// Only those two limits: the level's maximum bitrate is not checked.
///
/// Level 4 is the floor because it was the fixed level before, and decoders
/// take the DPB size from `max_dec_frame_buffering` rather than from the level,
/// so a lower one would buy nothing. Above 1080p (a 1440p or 4K screen share) a
/// fixed level 4 would declare a stream its decoder may refuse.
fn level(width: u32, height: u32, framerate: u32) -> Level {
	// H.264 Table A-1: (level, MaxMBPS, MaxFS).
	const LEVELS: [(Level, u64, u64); 8] = [
		(Level::L4, 245_760, 8_192),
		(Level::L4_2, 522_240, 8_704),
		(Level::L5, 589_824, 22_080),
		(Level::L5_1, 983_040, 36_864),
		(Level::L5_2, 2_073_600, 36_864),
		(Level::L6, 4_177_920, 139_264),
		(Level::L6_1, 8_355_840, 139_264),
		(Level::L6_2, 16_711_680, 139_264),
	];
	let frame = u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16));
	let rate = frame * u64::from(framerate.max(1));
	LEVELS
		.into_iter()
		.find(|&(_, max_rate, max_frame)| frame <= max_frame && rate <= max_rate)
		.map(|(level, _, _)| level)
		.unwrap_or(Level::L6_2)
}

/// Returns the coded buffer size in bytes for `bitrate`: two seconds of it, and at least 1.5 MB.
fn coded_size(bitrate: u32) -> usize {
	(bitrate as usize / 4).max(1_500_000)
}

/// Returns whether the VA fourcc `fourcc` is packed RGB, which has no YUV matrix of its own.
fn is_rgb(fourcc: u32) -> bool {
	matches!(
		fourcc,
		VA_FOURCC_BGRX | VA_FOURCC_BGRA | VA_FOURCC_RGBX | VA_FOURCC_RGBA | VA_FOURCC_XRGB | VA_FOURCC_ARGB
	)
}

/// Allocate one NV12 encode surface.
fn make_surface(display: &Arc<Display>, (width, height): (u32, u32)) -> anyhow::Result<Surface<()>> {
	let mut surfaces = display
		.create_surfaces::<()>(
			VA_RT_FORMAT_YUV420,
			Some(VA_FOURCC_NV12),
			width,
			height,
			Some(UsageHint::USAGE_HINT_ENCODER),
			vec![()],
		)
		.map_err(|e| anyhow::anyhow!("create surface: {e:?}"))?;
	surfaces.pop().ok_or_else(|| anyhow::anyhow!("no surface created"))
}

/// Upload tightly-packed NV12 into a surface. Ported from cros-codecs
/// `upload_nv12_img` (honors the image's per-plane offsets + pitches).
fn upload_nv12(
	display: &Arc<Display>,
	surface: &Surface<()>,
	width: u32,
	height: u32,
	data: &[u8],
) -> anyhow::Result<()> {
	let formats = display
		.query_image_formats()
		.map_err(|e| anyhow::anyhow!("query image formats: {e:?}"))?;
	let format = formats
		.into_iter()
		.find(|f| f.fourcc == VA_FOURCC_NV12)
		.ok_or_else(|| anyhow::anyhow!("driver has no NV12 image format"))?;

	let mut image = Image::create_from(surface, format, surface.size(), surface.size())
		.map_err(|e| anyhow::anyhow!("create image: {e:?}"))?;
	let va_image = *image.image();
	let dest: &mut [u8] = image.as_mut();
	let (w, h) = (width as usize, height as usize);

	// Luma plane.
	let mut src = data;
	let mut dst = &mut dest[va_image.offsets[0] as usize..];
	for _ in 0..h {
		dst[..w].copy_from_slice(&src[..w]);
		dst = &mut dst[va_image.pitches[0] as usize..];
		src = &src[w..];
	}
	// Interleaved chroma plane (h/2 rows of w bytes).
	let mut src = &data[w * h..];
	let mut dst = &mut dest[va_image.offsets[1] as usize..];
	for _ in 0..h / 2 {
		dst[..w].copy_from_slice(&src[..w]);
		dst = &mut dst[va_image.pitches[1] as usize..];
		src = &src[w..];
	}

	drop(image);
	surface.sync().map_err(|e| anyhow::anyhow!("surface sync: {e:?}"))?;
	Ok(())
}

/// Build the SPS/PPS for a low-latency IPPP stream. Ported from
/// discord/cros-codecs `LowDelayH264Delegate::new_sequence` (BSD-3-Clause).
fn build_sps_pps(config: &Config) -> (Rc<Sps>, Rc<Pps>) {
	let level = level(config.width, config.height, config.framerate);
	let (primaries, transfer, matrix) = config.color.vui();
	let sps = SpsBuilder::new()
		.seq_parameter_set_id(0)
		.profile_idc(Profile::Main)
		.chroma_format_idc(1)
		.level_idc(level)
		.max_frame_num(LIMIT)
		.pic_order_cnt_type(0)
		.max_pic_order_cnt_lsb(LIMIT * 2)
		.max_num_ref_frames(1)
		.frame_mbs_only_flag(true)
		.direct_8x8_inference_flag(level >= Level::L3)
		.resolution(config.width, config.height)
		.bit_depth_luma(8)
		.bit_depth_chroma(8)
		.aspect_ratio(1, 1)
		.timing_info(1, config.framerate * 2, false)
		.video_signal_type(config.color.full_range, primaries, transfer, matrix)
		.max_num_reorder_frames(0)
		.max_dec_frame_buffering(1)
		.build();

	let pps = PpsBuilder::new(Rc::clone(&sps))
		.pic_parameter_set_id(0)
		.pic_init_qp(26)
		.entropy_coding_mode_flag(true)
		.transform_8x8_mode_flag(false)
		.deblocking_filter_control_present_flag(true)
		.num_ref_idx_l0_default_active(1)
		.num_ref_idx_l1_default_active_minus1(0)
		.build();

	(sps, pps)
}

/// Returns the parameter buffers of one H.264 picture, then the packed headers the driver takes: the SPS and PPS on an IDR, and the slice header.
fn h264_buffers(sps: &Sps, pps: &Rc<Pps>, picture: &PictureInfo<'_>, packed: u32) -> anyhow::Result<Vec<BufferType>> {
	let meta = FrameMeta::new(picture.count);
	let slice_type = if picture.idr { SliceType::I } else { SliceType::P };
	let header = SliceHeaderBuilder::new(pps)
		.slice_type(slice_type)
		.first_mb_in_slice(0)
		.frame_num(meta.frame_num as u16)
		.pic_order_cnt_lsb((meta.poc % (LIMIT * 2)) as u16)
		// Only consecutive IDRs must differ.
		.idr_pic_id((picture.idr_id % 2) as u16)
		.build();
	let macroblocks = (sps.pic_width_in_mbs_minus1 as u32 + 1) * (sps.pic_height_in_map_units_minus1 as u32 + 1);
	let reference = picture
		.reference
		.map(|reference| (reference.surface, FrameMeta::new(reference.count)));

	let mut buffers = vec![
		build_enc_seq_param(sps, picture.bitrate, LIMIT, 0),
		build_enc_pic_param(pps, picture.coded, picture.recon, meta, picture.idr, reference),
		build_enc_slice_param(pps, &header, reference, macroblocks),
	];
	if picture.idr && packed & VA_ENC_PACKED_HEADER_SEQUENCE != 0 {
		buffers.extend(packed_parameter_sets(sps, pps)?);
	}
	if packed & VA_ENC_PACKED_HEADER_SLICE != 0 {
		let mut nal = Vec::new();
		let trailing = Synthesizer::<'_, SliceHeader, _>::synthesize(
			&header,
			sps,
			pps,
			picture.idr,
			IsReference::ShortTerm,
			&mut nal,
			true,
		)
		.map_err(|e| anyhow::anyhow!("synthesize slice header: {e:?}"))?;
		// The slice data follows the header's last bit, not its padded last byte.
		let bits = nal.len() * 8 - usize::from(trailing);
		buffers.extend(packed_header(EncPackedHeaderType::Slice, &nal, bits));
	}
	Ok(buffers)
}

/// Returns the SPS and PPS as packed headers, which go out with every IDR.
fn packed_parameter_sets(sps: &Sps, pps: &Pps) -> anyhow::Result<[BufferType; 4]> {
	let mut sps_nal = Vec::new();
	Synthesizer::<'_, Sps, _>::synthesize(3, sps, &mut sps_nal, true)
		.map_err(|e| anyhow::anyhow!("synthesize SPS: {e:?}"))?;
	let mut pps_nal = Vec::new();
	Synthesizer::<'_, Pps, _>::synthesize(3, pps, &mut pps_nal, true)
		.map_err(|e| anyhow::anyhow!("synthesize PPS: {e:?}"))?;

	let [sps_param, sps_data] = packed_header(EncPackedHeaderType::Sequence, &sps_nal, sps_nal.len() * 8);
	let [pps_param, pps_data] = packed_header(EncPackedHeaderType::Picture, &pps_nal, pps_nal.len() * 8);
	Ok([sps_param, sps_data, pps_param, pps_data])
}

fn build_invalid_pic() -> PictureH264 {
	PictureH264::new(VA_INVALID_ID, 0, VA_PICTURE_H264_INVALID, 0, 0)
}

fn build_h264_pic(surface_id: u32, meta: FrameMeta) -> PictureH264 {
	PictureH264::new(
		surface_id,
		meta.frame_num,
		VA_PICTURE_H264_SHORT_TERM_REFERENCE,
		meta.poc as i32,
		meta.poc as i32,
	)
}

/// Ported from cros-codecs `build_enc_seq_param` (BSD-3-Clause).
fn build_enc_seq_param(sps: &Sps, bits_per_second: u32, intra_period: u32, ip_period: u32) -> BufferType {
	let seq_fields = H264EncSeqFields::new(
		sps.chroma_format_idc as u32,
		sps.frame_mbs_only_flag as u32,
		sps.mb_adaptive_frame_field_flag as u32,
		sps.seq_scaling_matrix_present_flag as u32,
		sps.direct_8x8_inference_flag as u32,
		sps.log2_max_frame_num_minus4 as u32,
		sps.pic_order_cnt_type as u32,
		sps.log2_max_pic_order_cnt_lsb_minus4 as u32,
		sps.delta_pic_order_always_zero_flag as u32,
	);

	let frame_crop = if sps.frame_cropping_flag {
		Some(H264EncFrameCropOffsets::new(
			sps.frame_crop_left_offset,
			sps.frame_crop_right_offset,
			sps.frame_crop_top_offset,
			sps.frame_crop_bottom_offset,
		))
	} else {
		None
	};

	let vui_fields = if sps.vui_parameters_present_flag {
		Some(H264VuiFields::new(
			sps.vui_parameters.aspect_ratio_idc as u32,
			sps.vui_parameters.timing_info_present_flag as u32,
			sps.vui_parameters.bitstream_restriction_flag as u32,
			sps.vui_parameters.log2_max_mv_length_horizontal,
			sps.vui_parameters.log2_max_mv_length_vertical,
			sps.vui_parameters.fixed_frame_rate_flag as u32,
			sps.vui_parameters.low_delay_hrd_flag as u32,
			sps.vui_parameters.motion_vectors_over_pic_boundaries_flag as u32,
		))
	} else {
		None
	};

	let mut offset_for_ref_frame = [0i32; 256];
	offset_for_ref_frame[..255].copy_from_slice(&sps.offset_for_ref_frame[..]);

	BufferType::EncSequenceParameter(EncSequenceParameter::H264(EncSequenceParameterBufferH264::new(
		sps.seq_parameter_set_id,
		sps.level_idc as u8,
		intra_period,
		intra_period,
		ip_period,
		bits_per_second,
		sps.max_num_ref_frames as u32,
		sps.pic_width_in_mbs_minus1 + 1,
		sps.pic_height_in_map_units_minus1 + 1,
		&seq_fields,
		sps.bit_depth_luma_minus8,
		sps.bit_depth_chroma_minus8,
		sps.num_ref_frames_in_pic_order_cnt_cycle,
		sps.offset_for_non_ref_pic,
		sps.offset_for_top_to_bottom_field,
		offset_for_ref_frame,
		frame_crop,
		vui_fields,
		sps.vui_parameters.aspect_ratio_idc,
		sps.vui_parameters.sar_width as u32,
		sps.vui_parameters.sar_height as u32,
		sps.vui_parameters.num_units_in_tick,
		sps.vui_parameters.time_scale,
	)))
}

/// Ported from cros-codecs `build_enc_pic_param` (BSD-3-Clause).
fn build_enc_pic_param(
	pps: &Pps,
	coded: &EncCodedBuffer,
	recon_id: u32,
	meta: FrameMeta,
	is_idr: bool,
	reference: Option<(u32, FrameMeta)>,
) -> BufferType {
	let pic_fields = H264EncPicFields::new(
		is_idr as u32,
		1, // reference_pic_flag: this frame is a short-term reference
		pps.entropy_coding_mode_flag as u32,
		pps.weighted_pred_flag as u32,
		pps.weighted_bipred_idc as u32,
		pps.constrained_intra_pred_flag as u32,
		pps.transform_8x8_mode_flag as u32,
		pps.deblocking_filter_control_present_flag as u32,
		pps.redundant_pic_cnt_present_flag as u32,
		0,
		pps.pic_scaling_matrix_present_flag as u32,
	);

	let curr_pic = build_h264_pic(recon_id, meta);
	let mut reference_frames: [PictureH264; 16] = std::array::from_fn(|_| build_invalid_pic());
	if let Some((id, m)) = reference {
		reference_frames[0] = build_h264_pic(id, m);
	}

	BufferType::EncPictureParameter(EncPictureParameter::H264(EncPictureParameterBufferH264::new(
		curr_pic,
		reference_frames,
		coded.id(),
		pps.pic_parameter_set_id,
		pps.seq_parameter_set_id,
		0,
		meta.frame_num as u16,
		(pps.pic_init_qp_minus26 + 26) as u8,
		pps.num_ref_idx_l0_default_active_minus1,
		pps.num_ref_idx_l1_default_active_minus1,
		pps.chroma_qp_index_offset,
		pps.second_chroma_qp_index_offset,
		&pic_fields,
	)))
}

/// Ported from cros-codecs `build_enc_slice_param` (BSD-3-Clause), simplified for
/// IPPP (no weighted prediction, single reference in list 0).
fn build_enc_slice_param(
	pps: &Pps,
	header: &SliceHeader,
	reference: Option<(u32, FrameMeta)>,
	num_macroblocks: u32,
) -> BufferType {
	let mut ref_pic_list_0: [PictureH264; 32] = std::array::from_fn(|_| build_invalid_pic());
	if let Some((id, m)) = reference {
		ref_pic_list_0[0] = build_h264_pic(id, m);
	}
	let ref_pic_list_1: [PictureH264; 32] = std::array::from_fn(|_| build_invalid_pic());

	let (num_ref_idx_l0_active_minus1, num_ref_idx_l1_active_minus1) = if header.num_ref_idx_active_override_flag {
		(header.num_ref_idx_l0_active_minus1, header.num_ref_idx_l1_active_minus1)
	} else {
		(
			pps.num_ref_idx_l0_default_active_minus1,
			pps.num_ref_idx_l1_default_active_minus1,
		)
	};

	BufferType::EncSliceParameter(EncSliceParameter::H264(EncSliceParameterBufferH264::new(
		header.first_mb_in_slice,
		num_macroblocks,
		VA_INVALID_ID,
		header.slice_type as u8,
		pps.pic_parameter_set_id,
		header.idr_pic_id,
		header.pic_order_cnt_lsb,
		header.delta_pic_order_cnt_bottom,
		header.delta_pic_order_cnt,
		header.direct_spatial_mv_pred_flag as u8,
		header.num_ref_idx_active_override_flag as u8,
		num_ref_idx_l0_active_minus1,
		num_ref_idx_l1_active_minus1,
		ref_pic_list_0,
		ref_pic_list_1,
		header.pred_weight_table.luma_log2_weight_denom,
		header.pred_weight_table.chroma_log2_weight_denom,
		0,
		header.pred_weight_table.luma_weight_l0,
		[0i16; 32],
		0,
		header.pred_weight_table.chroma_weight_l0,
		[[0i16; 2]; 32],
		0,
		header.pred_weight_table.luma_weight_l1,
		[0i16; 32],
		0,
		header.pred_weight_table.chroma_weight_l1,
		[[0i16; 2]; 32],
		header.cabac_init_idc,
		header.slice_qp_delta,
		header.disable_deblocking_filter_idc,
		header.slice_alpha_c0_offset_div2,
		header.slice_beta_offset_div2,
	)))
}

/// Returns the packed-header parameter and data buffers for the first `bits` of `data`, a NAL unit that carries its own emulation prevention.
fn packed_header(kind: EncPackedHeaderType, data: &[u8], bits: usize) -> [BufferType; 2] {
	let param = BufferType::EncPackedHeaderParameter(EncPackedHeaderParameter::new(kind, bits as u32, true));
	let payload = BufferType::EncPackedHeaderData(data.to_vec());
	[param, payload]
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;
	use std::process::Command;
	use std::sync::atomic::{AtomicU32, Ordering};

	use super::*;
	use crate::dmabuf::DmaBuf;
	use crate::{VA_RT_FORMAT_RGB32, VA_RT_FORMAT_YUV420};

	// A size every driver encodes: radeonsi's H.265 minimum on VCN 5 is 384 wide.
	const WIDTH: u32 = 640;
	const HEIGHT: u32 = 480;
	const CODECS: [Codec; 2] = [Codec::H264, Codec::H265];

	fn config(codec: Codec) -> Config {
		Config {
			codec,
			..Config::new(WIDTH, HEIGHT, 30, 2_000_000, 30)
		}
	}

	/// Opens an encoder, or `None` to skip on a machine without VA-API encode of its codec.
	fn encoder(config: Config) -> Option<Encoder> {
		let codec = config.codec;
		match Encoder::new(config) {
			Ok(encoder) => Some(encoder),
			Err(err) => {
				eprintln!("skipping: no VA-API {codec:?} encoder: {err:#}");
				None
			}
		}
	}

	/// The NAL unit types that open an IDR access unit: the IDR slice, then the parameter sets it carries.
	fn idr_types(codec: Codec) -> (u8, &'static [u8]) {
		match codec {
			Codec::H264 => (5, &[7, 8]),
			Codec::H265 => (19, &[32, 33, 34]),
		}
	}

	/// A diagonal gradient with a block moving across it, so a stale or
	/// misplaced reference shows up as the block in the wrong place.
	fn nv12_frame(width: u32, height: u32, step: u32) -> Vec<u8> {
		let (w, h) = (width as usize, height as usize);
		let mut data = vec![128u8; w * h + w * h.div_ceil(2)];
		for y in 0..h {
			for x in 0..w {
				data[y * w + x] = (32 + (x + y) * 160 / (w + h)) as u8;
			}
		}
		let x0 = (step as usize * 8) % (w / 2);
		for y in h / 4..h / 2 {
			for x in x0..x0 + w / 4 {
				data[y * w + x] = 220;
			}
		}
		data
	}

	/// Returns a temporary file path unique to this call.
	///
	/// The tests run as threads of one process, so the process id alone would
	/// let two of them share a file.
	///
	/// Its extension names `codec`, for ffmpeg to pick the demuxer by.
	fn temp_path(codec: Codec, label: &str) -> PathBuf {
		static NEXT: AtomicU32 = AtomicU32::new(0);
		let index = NEXT.fetch_add(1, Ordering::Relaxed);
		let extension = match codec {
			Codec::H264 => "264",
			Codec::H265 => "265",
		};
		std::env::temp_dir().join(format!("moq-vaapi-{label}-{}-{index}.{extension}", std::process::id()))
	}

	/// Decodes an Annex-B stream with ffmpeg's software decoder into NV12
	/// pictures, or `None` to skip without ffmpeg.
	fn ffmpeg_decode(codec: Codec, stream: &[u8], name: &str) -> Option<Vec<u8>> {
		// ffmpeg reads the elementary stream from a file rather than a pipe, so
		// the test never has to drain its output while still writing its input.
		let path = temp_path(codec, name);
		std::fs::write(&path, stream).expect("write the elementary stream");
		let output = Command::new("ffmpeg")
			.args(["-v", "error", "-i"])
			.arg(&path)
			.args(["-pix_fmt", "nv12", "-f", "rawvideo", "-"])
			.output();
		let _ = std::fs::remove_file(&path);
		match output {
			Ok(output) if output.status.success() => Some(output.stdout),
			Ok(output) => panic!("ffmpeg refused the stream: {}", String::from_utf8_lossy(&output.stderr)),
			Err(_) => {
				eprintln!("skipping: no ffmpeg");
				None
			}
		}
	}

	/// Asks ffprobe for the stream's `color_range,color_space`.
	fn ffprobe_color(codec: Codec, stream: &[u8]) -> Option<String> {
		let path = temp_path(codec, "probe");
		std::fs::write(&path, stream).expect("write the elementary stream");
		let output = Command::new("ffprobe")
			.args([
				"-v",
				"error",
				"-show_entries",
				"stream=color_range,color_space",
				"-of",
				"csv=p=0",
			])
			.arg(&path)
			.output()
			.ok();
		let _ = std::fs::remove_file(&path);
		Some(String::from_utf8(output?.stdout).ok()?.trim().to_string())
	}

	/// Returns the peak signal-to-noise ratio of the luma planes, in dB.
	fn luma_psnr(a: &[u8], b: &[u8], width: u32, height: u32) -> f64 {
		let len = (width * height) as usize;
		let mse = a[..len]
			.iter()
			.zip(&b[..len])
			.map(|(&x, &y)| (x as f64 - y as f64).powi(2))
			.sum::<f64>()
			/ len as f64;
		10.0 * (255.0f64.powi(2) / mse.max(1e-9)).log10()
	}

	/// Returns the mean of the samples in a rectangle of one plane.
	fn mean(
		plane: &[u8],
		stride: usize,
		(x0, y0, x1, y1): (usize, usize, usize, usize),
		step: usize,
		at: usize,
	) -> f64 {
		let mut sum = 0.0;
		let mut count = 0.0;
		for y in y0..y1 {
			for x in x0..x1 {
				sum += plane[y * stride + x * step + at] as f64;
				count += 1.0;
			}
		}
		sum / count
	}

	/// Returns the NAL unit types in an Annex-B access unit.
	fn nal_types(codec: Codec, unit: &[u8]) -> Vec<u8> {
		let mut types = Vec::new();
		let mut i = 0;
		while i + 3 < unit.len() {
			if unit[i..i + 3] == [0, 0, 1] {
				types.push(match codec {
					Codec::H264 => unit[i + 3] & 0x1f,
					Codec::H265 => (unit[i + 3] >> 1) & 0x3f,
				});
				i += 3;
			}
			i += 1;
		}
		types
	}

	/// Returns a driver surface of `fourcc` on `display`, filled through an image.
	fn surface_with(
		display: &Arc<Display>,
		fourcc: u32,
		rt_format: u32,
		(width, height): (u32, u32),
		fill: impl FnOnce(&mut [u8], &crate::bindings::VAImage),
	) -> Surface<()> {
		let surface = display
			.create_surfaces(
				rt_format,
				Some(fourcc),
				width,
				height,
				Some(UsageHint::USAGE_HINT_EXPORT),
				vec![()],
			)
			.expect("allocate a surface")
			.pop()
			.expect("one surface");
		let format = display
			.query_image_formats()
			.expect("query image formats")
			.into_iter()
			.find(|f| f.fourcc == fourcc)
			.expect("the driver has an image format for the surface format");
		let mut image =
			Image::create_from(&surface, format, (width, height), (width, height)).expect("create an image");
		let va_image = *image.image();
		fill(image.as_mut(), &va_image);
		drop(image);
		surface.sync().expect("sync the filled surface");
		surface
	}

	/// Returns an NV12 surface holding `nv12_frame`, exported as a DMA-BUF.
	fn nv12_dmabuf(display: &Arc<Display>, (width, height): (u32, u32), step: u32) -> DmaBuf {
		let surface = surface_with(display, VA_FOURCC_NV12, VA_RT_FORMAT_YUV420, (width, height), |_, _| {});
		upload_nv12(display, &surface, width, height, &nv12_frame(width, height, step)).expect("upload");
		DmaBuf::from_prime(surface.export_prime().expect("export the surface")).expect("one object")
	}

	/// With no node named, the encoder opens one that encodes its codec and
	/// records which. A named node is used or refused, never swapped for another.
	#[test]
	fn the_render_node_is_found_or_named() {
		for codec in CODECS {
			let Some(encoder) = encoder(config(codec)) else {
				continue;
			};
			let device = encoder.config().device.clone().expect("the picked node is recorded");
			let display = Display::open_drm_display(&device).expect("reopen the picked node");
			probe(&display, codec).expect("the picked node encodes the codec");

			let named = Config {
				device: Some(PathBuf::from("/dev/null")),
				..config(codec)
			};
			assert!(
				Encoder::new(named).is_err(),
				"a named node that is not a render node was swapped for one that is"
			);
		}
	}

	/// CPU NV12 in, a stream out that an independent decoder reads back as the
	/// same pictures, at a size that is whole coding blocks and at one that is not.
	#[test]
	fn nv12_round_trips_through_a_software_decoder() {
		for codec in CODECS {
			for (width, height) in [(WIDTH, HEIGHT), (852, 478)] {
				let Some(mut encoder) = encoder(Config {
					width,
					height,
					..config(codec)
				}) else {
					continue;
				};
				let mut stream = Vec::new();
				let mut frames = Vec::new();
				for step in 0..10 {
					let frame = nv12_frame(width, height, step);
					stream.extend(encoder.encode_nv12(&frame, false).expect("encode"));
					frames.push(frame);
				}
				let Some(decoded) = ffmpeg_decode(codec, &stream, "nv12") else {
					return;
				};
				let len = frames[0].len();
				assert_eq!(
					decoded.len(),
					len * frames.len(),
					"{codec:?} at {width}x{height}: ffmpeg decoded a different count or size"
				);
				for (index, frame) in frames.iter().enumerate() {
					let psnr = luma_psnr(frame, &decoded[index * len..], width, height);
					assert!(
						psnr > 35.0,
						"{codec:?} at {width}x{height}: picture {index} decoded at {psnr:.1} dB"
					);
				}
			}
		}
	}

	/// The SPS names the color space the encoder was configured with.
	#[test]
	fn the_stream_is_labelled_with_its_color_space() {
		for codec in CODECS {
			for (color, expected) in [(Color::BT601, "tv,smpte170m"), (Color::BT709, "tv,bt709")] {
				let Some(mut encoder) = encoder(Config { color, ..config(codec) }) else {
					break;
				};
				let unit = encoder
					.encode_nv12(&nv12_frame(WIDTH, HEIGHT, 0), true)
					.expect("encode");
				let Some(found) = ffprobe_color(codec, &unit) else {
					eprintln!("skipping: no ffprobe");
					return;
				};
				assert_eq!(found, expected, "{codec:?}");
			}
		}
	}

	/// An IDR opens every group, and only there, carrying the parameter sets.
	#[test]
	fn keyframes_follow_the_group_length() {
		for codec in CODECS {
			let Some(mut encoder) = encoder(Config {
				gop: 10,
				..config(codec)
			}) else {
				continue;
			};
			let (idr_type, parameter_sets) = idr_types(codec);
			for step in 0..25 {
				let unit = encoder
					.encode_nv12(&nv12_frame(WIDTH, HEIGHT, step), false)
					.expect("encode");
				let types = nal_types(codec, &unit);
				let idr = types.contains(&idr_type);
				assert_eq!(idr, step % 10 == 0, "{codec:?} frame {step} has NAL types {types:?}");
				if idr {
					assert!(
						parameter_sets.iter().all(|kind| types.contains(kind)),
						"a {codec:?} IDR carries its parameter sets: {types:?}"
					);
				}
			}
			// A forced keyframe restarts the count: the next IDR is a group later.
			for step in 0..=10 {
				let unit = encoder
					.encode_nv12(&nv12_frame(WIDTH, HEIGHT, step), step == 0)
					.expect("encode");
				assert_eq!(
					nal_types(codec, &unit).contains(&idr_type),
					step % 10 == 0,
					"{codec:?} frame {step} after the forced keyframe"
				);
			}
		}
	}

	#[test]
	fn the_full_entrypoint_wins_and_low_power_is_the_fallback() {
		use VAEntrypoint::{
			VAEntrypointEncSlice as Full, VAEntrypointEncSliceLP as LowPower, VAEntrypointVLD as Decode,
		};
		assert_eq!(pick_entrypoint(&[Decode, LowPower, Full]), Some(Full));
		assert_eq!(pick_entrypoint(&[Decode, LowPower]), Some(LowPower));
		assert_eq!(pick_entrypoint(&[Decode]), None);
	}

	/// A buffer the driver refuses leaves the encoder usable, so the caller's
	/// CPU fallback can encode the same frame.
	#[test]
	fn a_refused_import_leaves_the_encoder_usable() {
		let Some(mut encoder) = encoder(config(Codec::H264)) else {
			return;
		};
		let Some(display) = Display::open() else {
			eprintln!("skipping: no VA-API display");
			return;
		};
		// A format with no mapping, and a real buffer under a modifier no driver has.
		let unmapped = DmaBuf {
			drm_format: u32::from_le_bytes(*b"ZZZZ"),
			..nv12_dmabuf(&display, (WIDTH, HEIGHT), 0)
		};
		assert!(encoder.encode_dmabuf(unmapped, true).is_err());
		let unknown_modifier = DmaBuf {
			modifier: 0x00ff_ffff_ffff_fffe,
			..nv12_dmabuf(&display, (WIDTH, HEIGHT), 0)
		};
		assert!(encoder.encode_dmabuf(unknown_modifier, true).is_err());

		let unit = encoder
			.encode_nv12(&nv12_frame(WIDTH, HEIGHT, 0), true)
			.expect("the CPU path still encodes");
		let Some(decoded) = ffmpeg_decode(Codec::H264, &unit, "refused") else {
			return;
		};
		let psnr = luma_psnr(&nv12_frame(WIDTH, HEIGHT, 0), &decoded, WIDTH, HEIGHT);
		assert!(psnr > 35.0, "decoded at {psnr:.1} dB");
	}

	/// A buffer short of whole coding blocks is not encoded in place: the
	/// encoder reads whole blocks, so it goes through the video processor into
	/// the padded input surface. That is 1080 rows for H.264, which reads 1088,
	/// and 1076 for H.265, whose minimum coding blocks are 8 or 16 rows.
	#[test]
	fn a_buffer_short_of_whole_coding_blocks_is_blitted() {
		for (codec, (width, height)) in [(Codec::H264, (1920, 1080)), (Codec::H265, (1920, 1076))] {
			let Some(mut encoder) = encoder(Config {
				codec,
				..Config::new(width, height, 30, 4_000_000, 30)
			}) else {
				continue;
			};
			let Some(display) = Display::open() else {
				eprintln!("skipping: no VA-API display");
				return;
			};
			let unit = encoder
				.encode_dmabuf(nv12_dmabuf(&display, (width, height), 0), true)
				.expect("encode the DMA-BUF");
			let Some(decoded) = ffmpeg_decode(codec, &unit, "short") else {
				return;
			};
			let frame = nv12_frame(width, height, 0);
			assert_eq!(decoded.len(), frame.len(), "{codec:?} decoded at another size");
			let psnr = luma_psnr(&frame, &decoded, width, height);
			assert!(psnr > 35.0, "{codec:?} decoded at {psnr:.1} dB");
		}
	}

	/// A YUV buffer labelled with another color space is converted into the
	/// stream's, not relabelled.
	#[test]
	fn a_yuv_dmabuf_in_another_space_is_converted() {
		for codec in CODECS {
			yuv_dmabuf_in_another_space_is_converted(codec);
		}
	}

	fn yuv_dmabuf_in_another_space_is_converted(codec: Codec) {
		let Some(mut encoder) = encoder(Config {
			color: Color::BT601,
			..config(codec)
		}) else {
			return;
		};
		let Some(display) = Display::open() else {
			eprintln!("skipping: no VA-API display");
			return;
		};
		// Flat red, written in BT.709.
		let [y, u, v] = limited([255, 0, 0], Color::BT709).map(|value| value.round() as u8);
		let surface = surface_with(
			&display,
			VA_FOURCC_NV12,
			VA_RT_FORMAT_YUV420,
			(WIDTH, HEIGHT),
			|data, image| {
				for row in 0..HEIGHT as usize {
					let at = image.offsets[0] as usize + row * image.pitches[0] as usize;
					data[at..at + WIDTH as usize].fill(y);
				}
				for row in 0..HEIGHT as usize / 2 {
					let at = image.offsets[1] as usize + row * image.pitches[1] as usize;
					for pair in data[at..at + WIDTH as usize].chunks_exact_mut(2) {
						pair.copy_from_slice(&[u, v]);
					}
				}
			},
		);
		let buffer = DmaBuf {
			color: Some(Color::BT709),
			..DmaBuf::from_prime(surface.export_prime().expect("export")).expect("one object")
		};
		let unit = encoder.encode_dmabuf(buffer, true).expect("encode");
		let Some(decoded) = ffmpeg_decode(codec, &unit, "yuv-color") else {
			return;
		};

		let (w, h) = (WIDTH as usize, HEIGHT as usize);
		let (luma, chroma) = decoded.split_at(w * h);
		let actual = [
			mean(luma, w, (w / 4, h / 4, w * 3 / 4, h * 3 / 4), 1, 0),
			mean(chroma, w, (w / 8, h / 8, w * 3 / 8, h * 3 / 8), 2, 0),
			mean(chroma, w, (w / 8, h / 8, w * 3 / 8, h * 3 / 8), 2, 1),
		];
		let expected = limited([255, 0, 0], Color::BT601);
		for (channel, (a, e)) in actual.iter().zip(expected).enumerate() {
			assert!(
				(a - e).abs() < 6.0,
				"{codec:?} channel {channel}: decoded {a:.1}, expected BT.601 {e:.1}"
			);
		}
	}

	/// An NV12 DMA-BUF at the encoder's size is encoded from the producer's memory.
	#[test]
	fn an_nv12_dmabuf_encodes_in_place() {
		for codec in CODECS {
			let Some(mut encoder) = encoder(config(codec)) else {
				continue;
			};
			let Some(display) = Display::open() else {
				eprintln!("skipping: no VA-API display");
				return;
			};
			let mut stream = Vec::new();
			for step in 0..5 {
				let buffer = nv12_dmabuf(&display, (WIDTH, HEIGHT), step);
				stream.extend(encoder.encode_dmabuf(buffer, false).expect("encode the DMA-BUF"));
			}
			let Some(decoded) = ffmpeg_decode(codec, &stream, "dmabuf") else {
				return;
			};
			let len = nv12_frame(WIDTH, HEIGHT, 0).len();
			assert_eq!(decoded.len(), 5 * len, "{codec:?}");
			for step in 0..5 {
				let psnr = luma_psnr(
					&nv12_frame(WIDTH, HEIGHT, step),
					&decoded[step as usize * len..],
					WIDTH,
					HEIGHT,
				);
				assert!(psnr > 35.0, "{codec:?} picture {step} decoded at {psnr:.1} dB");
			}
		}
	}

	/// An NV12 DMA-BUF at another size is scaled to the encoder's on the GPU.
	#[test]
	fn a_dmabuf_at_another_size_is_scaled() {
		for codec in CODECS {
			dmabuf_at_another_size_is_scaled(codec);
		}
	}

	fn dmabuf_at_another_size_is_scaled(codec: Codec) {
		let Some(mut encoder) = encoder(config(codec)) else {
			return;
		};
		let Some(display) = Display::open() else {
			eprintln!("skipping: no VA-API display");
			return;
		};
		let mut stream = Vec::new();
		for step in 0..3 {
			let buffer = nv12_dmabuf(&display, (WIDTH * 2, HEIGHT * 2), step);
			stream.extend(encoder.encode_dmabuf(buffer, false).expect("encode the DMA-BUF"));
		}
		let Some(decoded) = ffmpeg_decode(codec, &stream, "scaled") else {
			return;
		};
		let len = nv12_frame(WIDTH, HEIGHT, 0).len();
		assert_eq!(decoded.len(), 3 * len, "{codec:?}: the output is at the encoder's size");
		// Compare against the source scaled on the CPU by point sampling, which
		// differs from the GPU filter only at edges. 25 dB separates a scale
		// from a crop, which scores about 16 dB on this gradient.
		for step in 0..3u32 {
			let source = nv12_frame(WIDTH * 2, HEIGHT * 2, step);
			let mut expected = vec![0u8; (WIDTH * HEIGHT) as usize];
			for y in 0..HEIGHT as usize {
				for x in 0..WIDTH as usize {
					expected[y * WIDTH as usize + x] = source[(y * 2) * (WIDTH as usize * 2) + x * 2];
				}
			}
			let psnr = luma_psnr(&expected, &decoded[step as usize * len..], WIDTH, HEIGHT);
			assert!(psnr > 25.0, "{codec:?} picture {step} decoded at {psnr:.1} dB");
		}
	}

	/// A packed RGB DMA-BUF is converted on the GPU into the configured color space.
	#[test]
	fn an_rgb_dmabuf_is_converted_to_the_configured_space() {
		// Four flat quadrants, so the check reads colors rather than edges.
		const QUADRANTS: [[u8; 3]; 4] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 255]];
		for (codec, color) in CODECS
			.into_iter()
			.flat_map(|codec| [(codec, Color::BT601), (codec, Color::BT709)])
		{
			let Some(mut encoder) = encoder(Config { color, ..config(codec) }) else {
				continue;
			};
			let Some(display) = Display::open() else {
				eprintln!("skipping: no VA-API display");
				return;
			};
			let surface = surface_with(
				&display,
				VA_FOURCC_BGRX,
				VA_RT_FORMAT_RGB32,
				(WIDTH, HEIGHT),
				|data, image| {
					let pitch = image.pitches[0] as usize;
					let offset = image.offsets[0] as usize;
					for y in 0..HEIGHT as usize {
						for x in 0..WIDTH as usize {
							let quadrant = (y >= HEIGHT as usize / 2) as usize * 2 + (x >= WIDTH as usize / 2) as usize;
							let [r, g, b] = QUADRANTS[quadrant];
							let at = offset + y * pitch + x * 4;
							data[at..at + 4].copy_from_slice(&[b, g, r, 255]);
						}
					}
				},
			);
			let exported = surface.export_prime().expect("export");
			let buffer = DmaBuf::from_prime(exported).expect("one object");
			assert_eq!(buffer.drm_format, crate::dmabuf::drm::XRGB8888);
			let unit = encoder.encode_dmabuf(buffer, false).expect("encode the RGB DMA-BUF");
			let Some(decoded) = ffmpeg_decode(codec, &unit, "rgb") else {
				return;
			};

			let (w, h) = (WIDTH as usize, HEIGHT as usize);
			let (luma, chroma) = decoded.split_at(w * h);
			for (quadrant, rgb) in QUADRANTS.iter().enumerate() {
				let (qx, qy) = (quadrant % 2, quadrant / 2);
				// The middle half of each quadrant, clear of the chroma bleed at its edges.
				let area = |scale: usize| {
					let (qw, qh) = (w / 2 / scale, h / 2 / scale);
					(
						qx * qw + qw / 4,
						qy * qh + qh / 4,
						qx * qw + qw * 3 / 4,
						qy * qh + qh * 3 / 4,
					)
				};
				let actual = [
					mean(luma, w, area(1), 1, 0),
					mean(chroma, w, area(2), 2, 0),
					mean(chroma, w, area(2), 2, 1),
				];
				let expected = limited(*rgb, color);
				for (channel, (a, e)) in actual.iter().zip(expected).enumerate() {
					assert!(
						(a - e).abs() < 6.0,
						"{codec:?} {color:?} quadrant {quadrant} channel {channel}: decoded {a:.1}, expected {e:.1}"
					);
				}
			}
		}
	}

	#[test]
	fn the_level_covers_the_frame_size_and_rate() {
		assert_eq!(level(320, 240, 30), Level::L4);
		assert_eq!(level(1920, 1080, 30), Level::L4);
		assert_eq!(level(1920, 1080, 60), Level::L4_2);
		assert_eq!(level(2560, 1440, 30), Level::L5);
		assert_eq!(level(3840, 2160, 30), Level::L5_1);
		assert_eq!(level(3840, 2160, 60), Level::L5_2);
	}

	/// Returns the limited-range Y'CbCr of an RGB color, per BT.601 or BT.709.
	fn limited([r, g, b]: [u8; 3], color: Color) -> [f64; 3] {
		let (kr, kb) = match color.matrix {
			crate::Matrix::Bt601 => (0.299, 0.114),
			crate::Matrix::Bt709 => (0.2126, 0.0722),
		};
		let (r, g, b) = (r as f64, g as f64, b as f64);
		let y = kr * r + (1.0 - kr - kb) * g + kb * b;
		[
			16.0 + y * 219.0 / 255.0,
			128.0 + (b - y) / (2.0 * (1.0 - kb)) * 224.0 / 255.0,
			128.0 + (r - y) / (2.0 * (1.0 - kr)) * 224.0 / 255.0,
		]
	}

	/// Lowering the bitrate mid-stream shrinks the pictures without an IDR, and
	/// the rate controller holds the stream near each target.
	#[test]
	fn a_bitrate_change_takes_effect_without_a_keyframe() {
		for codec in CODECS {
			bitrate_change_takes_effect_without_a_keyframe(codec);
		}
	}

	fn bitrate_change_takes_effect_without_a_keyframe(codec: Codec) {
		let Some(mut encoder) = encoder(Config {
			codec,
			..Config::new(WIDTH, HEIGHT, 30, 2_000_000, 1_000)
		}) else {
			return;
		};
		let (idr_type, _) = idr_types(codec);
		// A zone plate whose phase moves every picture: detail motion
		// compensation cannot predict, so 2 Mbps is spent, yet the drivers
		// can code it at 250 kbps when the quantizer may go high enough.
		let frame = |step: u32| {
			let (w, h) = (WIDTH as usize, HEIGHT as usize);
			let mut frame = vec![128u8; w * h + w * h.div_ceil(2)];
			for y in 0..h {
				for x in 0..w {
					let (dx, dy) = (x as f64 - w as f64 / 2.0, y as f64 - h as f64 / 2.0);
					let phase = (dx * dx + dy * dy) * 0.002 + f64::from(step) * 0.5;
					frame[y * w + x] = (128.0 + 100.0 * phase.cos()) as u8;
				}
			}
			frame
		};
		// The mean picture size over the second half of 60 frames, once the
		// rate controller has settled.
		// Also asserts that no picture after the first is an IDR.
		let average = |encoder: &mut Encoder, from: u32| {
			let sizes: Vec<usize> = (from..from + 60)
				.map(|step| {
					let unit = encoder.encode_nv12(&frame(step), false).expect("encode");
					if step > 0 {
						assert!(
							!nal_types(codec, &unit).contains(&idr_type),
							"{codec:?} picture {step} is an IDR"
						);
					}
					unit.len()
				})
				.collect();
			sizes[30..].iter().sum::<usize>() / 30
		};

		// A settled stream within a quarter above its target; a quantizer
		// ceiling the rate controller cannot get past misses by double or more.
		let within = |bytes: usize, bitrate: u32| {
			let target = bitrate as usize / 8 / 30;
			assert!(
				bytes * 4 <= target * 5,
				"{codec:?} at {bitrate} bps: {bytes} bytes per picture, the target is {target}"
			);
		};

		let high = average(&mut encoder, 0);
		within(high, 2_000_000);
		encoder.set_bitrate(250_000).expect("set the bitrate");
		assert_eq!(encoder.config().bitrate, 250_000);
		assert!(encoder.set_bitrate(0).is_err(), "a zero bitrate is refused");
		let low = average(&mut encoder, 60);
		eprintln!("{codec:?} mean picture at 2 Mbps: {high} bytes, at 250 kbps: {low} bytes");
		within(low, 250_000);
		assert!(
			low * 3 < high,
			"{codec:?}: the rate did not drop: {high} -> {low} bytes per picture"
		);
	}
}
