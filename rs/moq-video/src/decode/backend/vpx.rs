//! Software VP8 and VP9 decode backend via libvpx.
//!
//! The portable path for both codecs, on every platform. Takes one coded frame
//! per call, exactly as a hang track carries it (a VP9 superframe stays one
//! unit), and returns packed I420.
//!
//! Only 8-bit 4:2:0 comes out: VP8, and VP9 profile 0. libvpx decodes the other
//! VP9 profiles too, but narrowing 4:4:4 or 10-bit samples to I420 would hand
//! back a different picture than the stream coded, so a picture in any other
//! format is refused rather than converted.

use std::ffi::CStr;
use std::ptr;

use bytes::Bytes;
use moq_net::Timestamp;
use vpx_sys::{
	VPX_DECODER_ABI_VERSION, vpx_codec_ctx_t, vpx_codec_dec_cfg_t, vpx_codec_dec_init_ver, vpx_codec_decode,
	vpx_codec_destroy, vpx_codec_err_t, vpx_codec_err_to_string, vpx_codec_error_detail, vpx_codec_get_frame,
	vpx_codec_iter_t, vpx_codec_vp8_dx, vpx_codec_vp9_dx, vpx_color_range, vpx_color_space, vpx_image_t, vpx_img_fmt,
};

use super::{Backend, Codec, Config};
use crate::frame::{I420, Surface};
use crate::{Color, Error, Frame, Size};

pub(crate) const NAME: &str = "vpx";

pub(crate) struct Vpx {
	ctx: vpx_codec_ctx_t,
	codec: Codec,
	/// A frame was refused, so the reference chain is broken until the next
	/// keyframe. Delta frames in between are dropped rather than decoded against
	/// references that are missing or wrong.
	broken: bool,
	/// Frames dropped since the chain broke, so a run of them logs once.
	lost: u64,
}

impl Vpx {
	pub(crate) fn open(codec: Codec, config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(codec, config)?))
	}

	/// libvpx has no scaler, so `config` only matters to the front end.
	fn new(codec: Codec, _config: &Config) -> Result<Self, Error> {
		// SAFETY: both return a pointer to a static interface table.
		let iface = unsafe {
			match codec {
				Codec::Vp8 => vpx_codec_vp8_dx(),
				Codec::Vp9 => vpx_codec_vp9_dx(),
				other => {
					return Err(Error::Codec(anyhow::anyhow!("{NAME} cannot decode {}", other.label())));
				}
			}
		};

		// libvpx bounds this itself: VP8 to 8 threads, VP9 to its tile columns.
		let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
		let cfg = vpx_codec_dec_cfg_t {
			threads: u32::try_from(threads).unwrap_or(1),
			w: 0,
			h: 0,
		};

		let mut ctx = vpx_codec_ctx_t::default();
		// SAFETY: `ctx` is zeroed and outlives the call; libvpx copies `cfg`.
		let err = unsafe { vpx_codec_dec_init_ver(&mut ctx, iface, &cfg, 0, VPX_DECODER_ABI_VERSION as i32) };
		if err != vpx_codec_err_t::VPX_CODEC_OK {
			return Err(Error::Codec(anyhow::anyhow!(
				"{NAME} {} decoder init: {}",
				codec.label(),
				describe(err, None)
			)));
		}

		tracing::info!(decoder = NAME, codec = codec.label(), "opened decoder");
		Ok(Self {
			ctx,
			codec,
			broken: false,
			lost: 0,
		})
	}

	/// The error libvpx reported for the last call, with its detail string when it
	/// left one.
	fn error(&self, err: vpx_codec_err_t) -> String {
		// SAFETY: the context is initialized, and the detail string is owned by it.
		let detail = unsafe {
			let detail = vpx_codec_error_detail(&self.ctx);
			(!detail.is_null()).then(|| CStr::from_ptr(detail).to_string_lossy().into_owned())
		};
		describe(err, detail)
	}

	/// Records a frame the decoder refused, or one dropped while waiting for a
	/// keyframe, and logs the first of a run.
	fn picture_lost(&mut self, reason: &str) {
		self.broken = true;
		self.lost += 1;
		if self.lost == 1 {
			tracing::warn!(
				decoder = NAME,
				codec = self.codec.label(),
				reason,
				"picture lost, waiting for the next keyframe"
			);
		} else {
			tracing::trace!(decoder = NAME, reason, "picture lost");
		}
	}
}

/// libvpx's name for `err`, plus the detail when there is one.
fn describe(err: vpx_codec_err_t, detail: Option<String>) -> String {
	// SAFETY: returns a pointer to a static string for every error code.
	let name = unsafe { CStr::from_ptr(vpx_codec_err_to_string(err)) }.to_string_lossy();
	match detail {
		Some(detail) => format!("{name}: {detail}"),
		None => name.into_owned(),
	}
}

/// Whether `err` describes the frame rather than the decoder.
///
/// A truncated or damaged frame is ordinary on a lossy path, and libvpx stays
/// usable after it: the next keyframe restores the picture. Anything else says
/// the stream or the decoder is in trouble, including `UNSUP_BITSTREAM`, which
/// libvpx documents as a stream it cannot parse at all and so cannot proceed on.
fn describes_frame(err: vpx_codec_err_t) -> bool {
	err == vpx_codec_err_t::VPX_CODEC_CORRUPT_FRAME
}

/// The color space libvpx reports for a picture, when [`Color`] can name it.
fn color(codec: Codec, img: &vpx_image_t) -> Option<Color> {
	let full = img.range == vpx_color_range::VPX_CR_FULL_RANGE;
	match (codec, img.cs) {
		// VP8 defines one color space (RFC 6386 section 9.2): BT.601, studio
		// swing. libvpx leaves `cs` unset for it.
		(Codec::Vp8, _) => Some(Color::Bt601Limited),
		(_, vpx_color_space::VPX_CS_BT_601 | vpx_color_space::VPX_CS_SMPTE_170) => Some(match full {
			true => Color::Bt601Full,
			false => Color::Bt601Limited,
		}),
		(_, vpx_color_space::VPX_CS_BT_709) => Some(match full {
			true => Color::Bt709Full,
			false => Color::Bt709Limited,
		}),
		_ => None,
	}
}

/// Copies one decoded picture out of libvpx's frame buffer.
fn picture(codec: Codec, img: &vpx_image_t, timestamp: Timestamp) -> Result<Frame, Error> {
	if img.fmt != vpx_img_fmt::VPX_IMG_FMT_I420 || img.bit_depth != 8 {
		return Err(Error::UnsupportedCodec(format!(
			"{} picture in {:?} at {} bits; only 8-bit 4:2:0 is supported",
			codec.label(),
			img.fmt,
			img.bit_depth
		)));
	}

	let size = Size::new(img.d_w, img.d_h);
	if !size.width.is_multiple_of(2) || !size.height.is_multiple_of(2) {
		return Err(Error::Codec(anyhow::anyhow!(
			"decoded frame has odd dimensions {size}, expected 4:2:0"
		)));
	}

	let (w, h) = (size.width as usize, size.height as usize);
	let mut data = vec![0u8; I420::len(size)?];
	let (luma, chroma) = data.split_at_mut(w * h);
	let (u, v) = chroma.split_at_mut(w * h / 4);
	for (plane, dst, width) in [(0, luma, w), (1, u, w / 2), (2, v, w / 2)] {
		let stride = img.stride[plane] as usize;
		for (row, dst) in dst.chunks_exact_mut(width).enumerate() {
			// SAFETY: libvpx guarantees every row of an 8-bit I420 picture holds at
			// least `width` samples, `stride` bytes after the one above it.
			let src = unsafe { std::slice::from_raw_parts(img.planes[plane].add(row * stride), width) };
			dst.copy_from_slice(src);
		}
	}

	let mut i420 = I420::new(size, data)?;
	i420.color = color(codec, img);
	Ok(Frame::new(Surface::I420(i420), timestamp))
}

impl Backend for Vpx {
	fn decode(&mut self, frame: Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Frame>, Error> {
		if self.broken {
			if !keyframe {
				self.picture_lost("reference chain broken");
				return Ok(Vec::new());
			}
			tracing::info!(decoder = NAME, lost = self.lost, "picture recovered");
			self.broken = false;
			self.lost = 0;
		}

		let len = u32::try_from(frame.len()).map_err(|_| {
			Error::Codec(anyhow::anyhow!(
				"{} frame of {} bytes is too large",
				self.codec.label(),
				frame.len()
			))
		})?;
		// SAFETY: `frame` outlives the call, and libvpx keeps no pointer into it.
		let err = unsafe { vpx_codec_decode(&mut self.ctx, frame.as_ptr(), len, ptr::null_mut(), 0) };
		if err != vpx_codec_err_t::VPX_CODEC_OK {
			let reason = self.error(err);
			if !describes_frame(err) {
				return Err(Error::Codec(anyhow::anyhow!("{NAME} decode: {reason}")));
			}
			self.picture_lost(&reason);
			return Ok(Vec::new());
		}

		// Without frame threading a call hands back at most the one picture it
		// shows, so the input timestamp is that picture's. A frame that is not
		// shown (a VP8 alt-ref coded on its own) hands back none.
		let mut frames = Vec::new();
		let mut iter: vpx_codec_iter_t = ptr::null();
		// SAFETY: each image stays valid until the next decode call.
		while let Some(img) = unsafe { vpx_codec_get_frame(&mut self.ctx, &mut iter).as_ref() } {
			frames.push(picture(self.codec, img, timestamp)?);
		}
		Ok(frames)
	}

	/// libvpx decodes without delay here, so nothing is ever held back.
	fn flush(&mut self) -> Result<Vec<Frame>, Error> {
		Ok(Vec::new())
	}

	fn name(&self) -> &str {
		NAME
	}
}

impl Drop for Vpx {
	fn drop(&mut self) {
		// SAFETY: the context was initialized in `new` and is destroyed once.
		unsafe { vpx_codec_destroy(&mut self.ctx) };
	}
}

#[cfg(test)]
mod tests {
	//! Decodes committed libvpx streams and compares every sample against ffmpeg's
	//! decode of the same stream. ffmpeg's VP8 and VP9 decoders are its own, not
	//! libvpx, and both formats decode bit-exactly, so the comparison is exact.
	//!
	//! Realtime, 30fps, a keyframe every 4 frames (I P P P I), generated with:
	//!
	//! ```text
	//! gen() { ffmpeg -f lavfi -i "testsrc2=s=$2:r=30" -frames:v 5 -c:v $1 \
	//!     -deadline realtime -cpu-used 8 -lag-in-frames 0 -g 4 -keyint_min 4 \
	//!     -b:v 200k -pix_fmt yuv420p -f ivf $3
	//!     ffmpeg -i $3 -f rawvideo -pix_fmt yuv420p ${3%.ivf}.yuv; }
	//! gen libvpx     64x64  vp8_64x64_pattern_5f.ivf
	//! gen libvpx     100x66 vp8_100x66_pattern_5f.ivf
	//! gen libvpx-vp9 64x64  vp9_64x64_pattern_5f.ivf
	//! gen libvpx-vp9 100x66 vp9_100x66_pattern_5f.ivf
	//! ffmpeg -f lavfi -i "testsrc2=s=16x16:r=30" -frames:v 1 -c:v libvpx-vp9 \
	//!     -deadline realtime -pix_fmt yuv444p -f ivf vp9_profile1_16x16_1f.ivf
	//! ```

	use hang::catalog::{VP9, VideoCodec, VideoConfig};

	use super::*;
	use crate::decode::{Decoder, Kind};

	/// A committed stream and the pictures it has to decode to.
	struct Vector {
		name: &'static str,
		codec: Codec,
		ivf: &'static [u8],
		yuv: &'static [u8],
		width: u32,
		height: u32,
	}

	impl Vector {
		fn size(&self) -> Size {
			Size::new(self.width, self.height)
		}
	}

	const VP8_64: Vector = Vector {
		name: "vp8_64x64_pattern_5f",
		codec: Codec::Vp8,
		ivf: include_bytes!("../test_data/vp8_64x64_pattern_5f.ivf"),
		yuv: include_bytes!("../test_data/vp8_64x64_pattern_5f.yuv"),
		width: 64,
		height: 64,
	};

	/// Not a multiple of the 16-pixel macroblock, so a decoder that hands back the
	/// coded size instead of the display size fails.
	const VP8_100: Vector = Vector {
		name: "vp8_100x66_pattern_5f",
		codec: Codec::Vp8,
		ivf: include_bytes!("../test_data/vp8_100x66_pattern_5f.ivf"),
		yuv: include_bytes!("../test_data/vp8_100x66_pattern_5f.yuv"),
		width: 100,
		height: 66,
	};

	const VP9_64: Vector = Vector {
		name: "vp9_64x64_pattern_5f",
		codec: Codec::Vp9,
		ivf: include_bytes!("../test_data/vp9_64x64_pattern_5f.ivf"),
		yuv: include_bytes!("../test_data/vp9_64x64_pattern_5f.yuv"),
		width: 64,
		height: 64,
	};

	const VP9_100: Vector = Vector {
		name: "vp9_100x66_pattern_5f",
		codec: Codec::Vp9,
		ivf: include_bytes!("../test_data/vp9_100x66_pattern_5f.ivf"),
		yuv: include_bytes!("../test_data/vp9_100x66_pattern_5f.yuv"),
		width: 100,
		height: 66,
	};

	const VECTORS: &[&Vector] = &[&VP8_64, &VP8_100, &VP9_64, &VP9_100];

	/// VP9 profile 1: 8-bit 4:4:4, which has no I420 form.
	const VP9_PROFILE1: &[u8] = include_bytes!("../test_data/vp9_profile1_16x16_1f.ivf");

	/// The interval between fixture pictures: 30fps.
	const FRAME_MICROS: u64 = 33_333;

	fn at(index: usize) -> Timestamp {
		Timestamp::from_micros(index as u64 * FRAME_MICROS).expect("fixture timestamp")
	}

	/// Splits an IVF file into its frames: a 32-byte file header, then each frame
	/// behind a 4-byte little-endian size and an 8-byte timestamp.
	fn frames(ivf: &'static [u8]) -> Vec<Bytes> {
		assert_eq!(&ivf[..4], b"DKIF", "not an IVF file");
		let header = u16::from_le_bytes([ivf[6], ivf[7]]) as usize;
		let ivf = Bytes::from_static(ivf);
		let mut frames = Vec::new();
		let mut pos = header;
		while pos < ivf.len() {
			let size = u32::from_le_bytes(ivf[pos..pos + 4].try_into().unwrap()) as usize;
			pos += 12;
			frames.push(ivf.slice(pos..pos + size));
			pos += size;
		}
		frames
	}

	/// Whether a frame is a keyframe, read from its first byte: VP8's
	/// `frame_type` is bit 0 (RFC 6386 section 9.1), and VP9 profile 0 and 1 put
	/// `show_existing_frame` and `frame_type` at bits 3 and 2 (VP9 section 6.2).
	fn keyframe(codec: Codec, frame: &[u8]) -> bool {
		match codec {
			Codec::Vp8 => frame[0] & 0x01 == 0,
			_ => frame[0] & 0x0c == 0,
		}
	}

	fn open(codec: Codec) -> Vpx {
		Vpx::new(codec, &Config::new()).expect("libvpx always opens")
	}

	/// The `index`th reference picture of `vector`.
	fn reference(vector: &Vector, index: usize) -> &'static [u8] {
		let len = I420::len(vector.size()).unwrap();
		&vector.yuv[index * len..(index + 1) * len]
	}

	/// Decodes `frames` in order, replacing the payload at `index` where `damage`
	/// says to, and returns each picture with the index of the frame it came from.
	fn decode(decoder: &mut Vpx, vector: &Vector, damage: impl Fn(usize, Bytes) -> Bytes) -> Vec<(usize, Frame)> {
		let mut out = Vec::new();
		for (index, frame) in frames(vector.ivf).into_iter().enumerate() {
			let key = keyframe(vector.codec, &frame);
			let pictures = decoder
				.decode(damage(index, frame), at(index), key)
				.unwrap_or_else(|e| panic!("{} frame {index} ended the stream: {e}", vector.name));
			out.extend(pictures.into_iter().map(|picture| (index, picture)));
		}
		out.extend(
			decoder
				.flush()
				.unwrap()
				.into_iter()
				.map(|picture| (usize::MAX, picture)),
		);
		out
	}

	fn assert_reference(vector: &Vector, index: usize, frame: &Frame) {
		let Surface::I420(i420) = &frame.surface else {
			panic!("{} produced a non-I420 surface", vector.name);
		};
		assert_eq!(
			(i420.width, i420.height),
			(vector.width, vector.height),
			"{}",
			vector.name
		);
		assert!(
			i420.data == reference(vector, index),
			"{} picture {index} differs from the reference decode",
			vector.name
		);
		assert_eq!(
			frame.timestamp,
			at(index),
			"{} picture {index} mis-stamped",
			vector.name
		);
	}

	#[test]
	fn every_picture_matches_the_reference() {
		for vector in VECTORS {
			let pictures = decode(&mut open(vector.codec), vector, |_, frame| frame);
			assert_eq!(pictures.len(), 5, "{} lost pictures", vector.name);
			for (index, frame) in &pictures {
				assert_reference(vector, *index, frame);
			}
		}
	}

	/// VP8 has exactly one color space; VP9 names its own, and these fixtures
	/// leave it unspecified.
	#[test]
	fn color_comes_from_the_bitstream() {
		let color = |vector: &Vector| {
			let (_, frame) = decode(&mut open(vector.codec), vector, |_, frame| frame).remove(0);
			frame.surface.color()
		};
		assert_eq!(color(&VP8_64), Some(Color::Bt601Limited));
		assert_eq!(color(&VP9_64), None);
	}

	/// A keyframe at a new size switches the decoder over without reopening it.
	#[test]
	fn a_keyframe_changes_the_resolution() {
		for (first, second) in [(&VP8_64, &VP8_100), (&VP9_64, &VP9_100), (&VP9_100, &VP9_64)] {
			let mut decoder = open(first.codec);
			for vector in [first, second] {
				let pictures = decode(&mut decoder, vector, |_, frame| frame);
				assert_eq!(pictures.len(), 5, "{} lost pictures after a switch", vector.name);
				for (index, frame) in &pictures {
					assert_reference(vector, *index, frame);
				}
			}
		}
	}

	/// A damaged delta frame costs pictures up to the next keyframe, not the
	/// stream, and nothing decoded against the broken references comes out.
	#[test]
	fn a_damaged_frame_recovers_at_the_next_keyframe() {
		for vector in VECTORS {
			let damaged = 1;
			// Keep the first bytes, so the frame still parses as a delta frame and
			// the damage is found inside it.
			let pictures = decode(&mut open(vector.codec), vector, |index, frame| match index == damaged {
				true => {
					let mut garbage = frame[..4].to_vec();
					garbage.extend(std::iter::repeat_n(0xa5, frame.len() / 2));
					Bytes::from(garbage)
				}
				false => frame,
			});

			let indices: Vec<usize> = pictures.iter().map(|(index, _)| *index).collect();
			assert_eq!(indices, [0, 4], "{} decoded across the damage", vector.name);
			for (index, frame) in &pictures {
				assert_reference(vector, *index, frame);
			}
		}
	}

	/// A frame libvpx cannot parse at all ends the stream, rather than blanking
	/// the picture while each later keyframe fails the same way.
	#[test]
	fn an_unparseable_frame_is_fatal() {
		for vector in [&VP8_64, &VP9_64] {
			let mut frame = frames(vector.ivf)[0].to_vec();
			match vector.codec {
				// The keyframe start code (RFC 6386 section 9.1).
				Codec::Vp8 => frame[3] = 0,
				// The two-bit frame marker (VP9 section 6.2).
				_ => frame[0] = 0,
			}
			match open(vector.codec).decode(Bytes::from(frame), at(0), true) {
				Err(Error::Codec(_)) => {}
				Err(other) => panic!("{}: expected Codec, got {other:?}", vector.name),
				Ok(frames) => panic!("{}: decoded {} pictures", vector.name, frames.len()),
			}
		}
	}

	/// A decoder that has been flushed takes a new stream from its keyframe.
	#[test]
	fn a_flushed_decoder_takes_another_stream() {
		let mut decoder = open(Codec::Vp9);
		for _ in 0..2 {
			let pictures = decode(&mut decoder, &VP9_64, |_, frame| frame);
			assert_eq!(pictures.len(), 5);
		}
	}

	/// A profile that decodes to something other than 8-bit 4:2:0 is refused, not
	/// narrowed, even when the catalog claimed profile 0.
	#[test]
	fn a_picture_that_is_not_i420_is_refused() {
		let frame = frames(VP9_PROFILE1).remove(0);
		match open(Codec::Vp9).decode(frame, at(0), true) {
			Err(Error::UnsupportedCodec(reason)) => assert!(reason.contains("I444"), "{reason}"),
			Err(other) => panic!("expected UnsupportedCodec, got {other:?}"),
			Ok(frames) => panic!("expected UnsupportedCodec, decoded {} pictures", frames.len()),
		}
	}

	/// The catalog is checked before anything is opened, so a VP9 track in a
	/// profile this backend cannot represent fails at subscribe time.
	#[test]
	fn the_catalog_gates_vp9_profiles() {
		let vp9 = |profile, bit_depth, chroma_subsampling| {
			VideoConfig::new(VideoCodec::VP9(VP9 {
				profile,
				level: 10,
				bit_depth,
				chroma_subsampling,
				..VP9::default()
			}))
		};
		let config = crate::decode::Config {
			kind: Kind::Named(NAME.to_owned()),
			..crate::decode::Config::new()
		};

		for catalog in [vp9(0, 8, 1), vp9(0, 8, 0), VideoConfig::new(VideoCodec::VP8)] {
			let decoder = Decoder::new(&catalog, &config).expect("8-bit 4:2:0 opens");
			assert_eq!(decoder.name(), NAME);
		}
		for catalog in [vp9(1, 8, 3), vp9(2, 10, 1), vp9(0, 8, 2)] {
			match Decoder::new(&catalog, &config) {
				Err(Error::UnsupportedCodec(codec)) => assert!(codec.starts_with("vp09."), "{codec}"),
				Err(other) => panic!("expected UnsupportedCodec, got {other:?}"),
				Ok(_) => panic!("{} opened", catalog.codec),
			}
		}
	}

	/// The front end passes VP9 frames through untouched and gates on the first
	/// keyframe, so a subscriber that joins mid-group waits instead of decoding
	/// against nothing.
	#[test]
	fn the_front_end_waits_for_a_keyframe() {
		let catalog = VideoConfig::new(VideoCodec::VP9(VP9 {
			profile: 0,
			level: 10,
			bit_depth: 8,
			..VP9::default()
		}));
		let config = crate::decode::Config {
			kind: Kind::Software,
			output: crate::Output::Cpu,
			..crate::decode::Config::new()
		};
		let mut decoder = Decoder::new(&catalog, &config).expect("software VP9 decoder");

		let mut decoded = Vec::new();
		// Join after the first keyframe: frames 1..3 are deltas, 4 a keyframe.
		for (index, frame) in frames(VP9_64.ivf).into_iter().enumerate().skip(1) {
			let key = keyframe(Codec::Vp9, &frame);
			decoded.extend(
				decoder
					.decode(&frame, at(index), key)
					.unwrap()
					.into_iter()
					.map(|f| (index, f)),
			);
		}
		assert_eq!(decoded.len(), 1, "a delta frame was decoded before the keyframe");
		let (index, frame) = &decoded[0];
		assert_reference(&VP9_64, *index, frame);
	}
}
