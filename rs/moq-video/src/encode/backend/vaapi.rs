//! Intel/AMD VAAPI hardware backend via the `moq-vaapi` crate on Linux.
//!
//! `moq-vaapi` is a focused VA-API H.264 encoder vendored and trimmed from
//! cros-libva + discord/cros-codecs. It emits an Annex-B elementary stream with
//! in-band SPS/PPS, matching avc3 mode, and labels the color space in the VUI.
//!
//! As of moq-vaapi 0.0.3 libva is `dlopen`'d at runtime, so a VAAPI-enabled build
//! needs no libva at build time and the binary carries no `NEEDED libva`. A
//! libva-less host, or a present-but-unusable VA stack (no render node, no usable
//! driver), makes `Encoder::new` return an error; under automatic selection
//! [`backend::open`](super::open) then moves on to openh264, like the NVENC backend.
//! The render node is the one the decoder and the GPU resize share, which
//! `MOQ_VAAPI_DEVICE` can name; see `frame::vaapi::device`.
//!
//! A [`Surface::DmaBuf`] is encoded without touching the CPU. An NV12 buffer at
//! the encoder's size (a VA-API decode, or one [`Surface::resize`] already
//! scaled on the GPU) is imported and encoded in place when its size is a
//! whole number of macroblocks. Anything else the driver imports, packed RGB
//! from a PipeWire screen capture above all, goes through the video processor
//! into the encoder's own surface first. RGB and YUV labelled with another
//! space are converted into the stream's; unlabelled YUV is taken to be in it.
//!
//! When the driver refuses to encode a buffer, the backend says so once and
//! sends every later buffer of the same format and modifier through the CPU
//! path. A failure before the driver sees the buffer, such as a producer fence
//! that times out, affects that frame only.
//!
//! Every other surface takes the CPU path: [`Surface::to_i420`], interleaved to
//! NV12, and uploaded.
//!
//! Validated on Intel Meteor Lake with iHD 26.1.5: the tests below encode CPU
//! frames, VA-API decodes handed over as DMA-BUFs, and packed RGB buffers at
//! and above the encoder's size, decode each stream with openh264, and compare
//! pixels. They skip on a machine without a VA-API device.

use std::collections::HashSet;
use std::path::Path;

use bytes::Bytes;
use moq_vaapi::encode::{Config as VaapiConfig, Encoder};

use super::super::encoder::{Config, Gop};
use super::{Backend, Encoded};
use crate::frame::{DmaBuf, DrmFormat, I420, vaapi};
use crate::{Error, Frame, Surface};

pub(crate) const NAME: &str = "vaapi";

pub(crate) struct Vaapi {
	encoder: Encoder,
	/// Buffer layouts the driver refused to encode, by format and modifier, so
	/// each costs one failed attempt rather than one per frame.
	refused: HashSet<(DrmFormat, u64)>,
	/// The CPU path's NV12 staging buffer, kept so a frame reuses the last one's
	/// allocation.
	nv12: Vec<u8>,
	/// Frames encoded from a DMA-BUF on the GPU, for the tests to tell the two
	/// paths apart.
	#[cfg(test)]
	gpu_frames: u64,
}

impl Vaapi {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		Ok(Box::new(Self::new(config)?))
	}

	fn new(config: &Config) -> Result<Self, Error> {
		let bitrate = config.resolved_bitrate().as_bps().min(u32::MAX as u64) as u32;
		let Gop::Keyframe { interval } = config.gop;
		let vaapi = VaapiConfig {
			device: vaapi::device().map(Path::to_path_buf),
			color: vaapi::color(config.resolved_color()),
			..VaapiConfig::new(
				config.width,
				config.height,
				config.framerate.rounded(),
				bitrate,
				interval,
			)
		};
		let encoder = Encoder::new(vaapi).map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI encoder init: {e:?}")))?;

		tracing::info!(
			encoder = NAME,
			device = ?encoder.config().device,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Self {
			encoder,
			refused: HashSet::new(),
			nv12: Vec::new(),
			#[cfg(test)]
			gpu_frames: 0,
		})
	}

	/// Encodes `buffer` on the GPU, or returns `None` when it has to go through the CPU instead.
	///
	/// Holds the producer's lease until the encode has read the buffer.
	fn encode_dmabuf(&mut self, buffer: &DmaBuf, cut: bool) -> Option<Vec<u8>> {
		let key = (buffer.format(), buffer.modifier());
		if self.refused.contains(&key) {
			return None;
		}
		let (descriptor, lease) = match vaapi::import(buffer) {
			Ok(imported) => imported,
			Err(err) => {
				tracing::debug!(encoder = NAME, %err, "DMA-BUF export failed; encoding this frame through the CPU");
				return None;
			}
		};
		let annexb = self.encoder.encode_dmabuf(descriptor, cut);
		drop(lease);
		match annexb {
			Ok(annexb) => {
				#[cfg(test)]
				{
					self.gpu_frames += 1;
				}
				Some(annexb)
			}
			Err(err) => {
				tracing::warn!(
					encoder = NAME,
					format = ?key.0,
					modifier = format_args!("{:#x}", key.1),
					err = format!("{err:#}"),
					"VAAPI cannot encode this DMA-BUF layout on the GPU; encoding it through the CPU from now on"
				);
				self.refused.insert(key);
				None
			}
		}
	}

	fn encode_cpu(&mut self, frame: &Frame, cut: bool) -> Result<Vec<u8>, Error> {
		let i420 = frame.surface.to_i420()?;
		i420_to_nv12(&i420, &mut self.nv12);
		self.encoder
			.encode_nv12(&self.nv12, cut)
			.map_err(|e| Error::Codec(e.context("VAAPI encode")))
	}
}

impl Backend for Vaapi {
	fn encode(&mut self, frame: &Frame, cut: bool) -> Result<Vec<Encoded>, Error> {
		let gpu = match &frame.surface {
			Surface::DmaBuf(buffer) => self.encode_dmabuf(buffer, cut),
			_ => None,
		};
		let annexb = match gpu {
			Some(annexb) => annexb,
			None => self.encode_cpu(frame, cut)?,
		};

		// Submitted and read back within the call, so this is that frame's output.
		Ok(if annexb.is_empty() {
			Vec::new()
		} else {
			vec![Encoded::new(Bytes::from(annexb), frame.timestamp)]
		})
	}

	fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
		// The encoder submits and reads back synchronously per frame, so nothing
		// is ever buffered.
		Ok(Vec::new())
	}

	fn finish(&mut self) -> Result<Vec<Encoded>, Error> {
		// The encoder submits and reads back synchronously per frame, so nothing
		// is buffered at shutdown.
		Ok(Vec::new())
	}

	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		// The rate control parameters go out with every frame, so the new rate
		// applies from the next one without an IDR.
		self.encoder
			.set_bitrate(bitrate.min(u32::MAX as u64) as u32)
			.map_err(|e| Error::Codec(e.context("VAAPI bitrate change")))
	}

	fn can_cut(&self) -> bool {
		true
	}

	fn name(&self) -> &'static str {
		NAME
	}
}

/// Interleave tightly-packed I420 into tightly-packed NV12 in `out`: copy Y
/// as-is, then interleave U and V into the chroma plane.
///
/// `out` is resized to the frame and every byte of it is written, so it can be
/// reused from frame to frame without clearing.
fn i420_to_nv12(i420: &I420, out: &mut Vec<u8>) {
	let (w, h) = (i420.width as usize, i420.height as usize);
	let (cw, ch) = (w / 2, h / 2);

	out.resize(w * h + 2 * cw * ch, 0);
	let (y, uv) = out.split_at_mut(w * h);
	y.copy_from_slice(i420.y());
	for ((pair, &u), &v) in uv.as_chunks_mut::<2>().0.iter_mut().zip(i420.u()).zip(i420.v()) {
		*pair = [u, v];
	}
}

// The round trips decode with openh264, an independent decoder.
#[cfg(all(test, feature = "openh264"))]
mod tests {
	use moq_net::Timestamp;

	use super::*;
	use crate::decode::backend as decode_backend;
	use crate::decode::{Codec as DecodeCodec, Config as DecodeConfig, Kind as DecodeKind};
	use crate::encode::{Encoder as CrateEncoder, Kind as EncodeKind};
	use crate::{Output, Rate, Size};

	const WIDTH: u32 = 320;
	const HEIGHT: u32 = 240;

	fn config() -> Config {
		Config {
			kind: EncodeKind::Named(NAME.into()),
			..Config::new(WIDTH, HEIGHT, Rate::new(30, 1).unwrap())
		}
	}

	/// Real hardware only: the backend itself, so a test can see which path
	/// it took, or `None` to skip on a box with no VA-API H.264 encoder.
	fn backend() -> Option<Vaapi> {
		match Vaapi::new(&config()) {
			Ok(backend) => Some(backend),
			Err(err) => {
				eprintln!("skipping: no VA-API H.264 encoder: {err}");
				None
			}
		}
	}

	fn decoder(name: &str, output: Output) -> Box<dyn decode_backend::Backend> {
		let config = DecodeConfig {
			kind: DecodeKind::Named(name.into()),
			output,
			..DecodeConfig::new()
		};
		decode_backend::open(DecodeCodec::H264, &config).expect("open the decoder")
	}

	/// A static RGBA gradient that varies in both axes, so the chroma planes have
	/// spatial structure and a pitch or plane-split bug corrupts the picture.
	fn gradient_rgba(size: Size) -> Vec<u8> {
		let (w, h) = (size.width as usize, size.height as usize);
		let mut rgba = vec![0u8; w * h * 4];
		for y in 0..h {
			for x in 0..w {
				let i = (y * w + x) * 4;
				rgba[i] = (x * 255 / w) as u8;
				rgba[i + 1] = (y * 255 / h) as u8;
				rgba[i + 2] = ((x + y) * 255 / (w + h)) as u8;
				rgba[i + 3] = 255;
			}
		}
		rgba
	}

	/// Mean absolute difference between two planes.
	fn mae(a: &[u8], b: &[u8]) -> u64 {
		assert_eq!(a.len(), b.len());
		a.iter().zip(b).map(|(&x, &y)| x.abs_diff(y) as u64).sum::<u64>() / a.len() as u64
	}

	fn at(index: u64) -> Timestamp {
		Timestamp::from_micros(index * 33_333).unwrap()
	}

	/// Encodes each frame with `backend` and decodes the stream with openh264,
	/// an independent decoder.
	fn round_trip(backend: &mut Vaapi, frames: &[Frame]) -> Vec<Frame> {
		let mut software = decoder("openh264", Output::Cpu);
		let mut decoded = Vec::new();
		for (index, frame) in frames.iter().enumerate() {
			for encoded in backend.encode(frame, index == 0).expect("encode") {
				decoded.extend(
					software
						.decode(encoded.payload, encoded.timestamp, index == 0)
						.expect("decode"),
				);
			}
		}
		decoded.extend(software.flush().expect("flush"));
		assert_eq!(decoded.len(), frames.len(), "pictures went missing");
		decoded
	}

	/// Asserts every plane of `decoded` is within `tolerance` of `expected` on average.
	///
	/// A lossy round trip of this smooth gradient lands within 2 to 4 code
	/// values; 8 (10 across two generations of coding) leaves room for encoder
	/// variation while a swapped chroma plane or a stride bug misses by 30 or
	/// more.
	fn assert_close(decoded: &Frame, expected: &I420, tolerance: u64) {
		let i420 = decoded.surface.to_i420().unwrap();
		assert_eq!((i420.width(), i420.height()), (expected.width(), expected.height()));
		for (plane, (a, b)) in [
			(i420.y(), expected.y()),
			(i420.u(), expected.u()),
			(i420.v(), expected.v()),
		]
		.into_iter()
		.enumerate()
		{
			let error = mae(a, b);
			assert!(error < tolerance, "plane {plane} is off by {error} on average");
		}
	}

	/// The reused staging buffer takes each frame's size and carries nothing
	/// over from a larger frame before it. Needs no hardware.
	#[test]
	fn the_nv12_staging_buffer_is_rewritten_per_frame() {
		let mut nv12 = Vec::new();
		let large = Size::new(64, 64);
		let large = I420::from_rgba(&gradient_rgba(large), large.width * 4, large).unwrap();
		i420_to_nv12(&large, &mut nv12);

		let small = Size::new(32, 16);
		let small = I420::from_rgba(&gradient_rgba(small), small.width * 4, small).unwrap();
		i420_to_nv12(&small, &mut nv12);

		let luma = small.y().len();
		assert_eq!(nv12.len(), luma * 3 / 2);
		assert_eq!(&nv12[..luma], small.y());
		for (index, pair) in nv12[luma..].as_chunks::<2>().0.iter().enumerate() {
			assert_eq!(*pair, [small.u()[index], small.v()[index]], "chroma pair {index}");
		}
	}

	/// CPU frames come back from an independent decoder as the picture that
	/// went in, which a stride or chroma-order bug in the upload would break.
	#[test]
	fn cpu_frames_round_trip_through_a_software_decoder() {
		let Some(mut backend) = backend() else { return };
		let size = Size::new(WIDTH, HEIGHT);
		let rgba = gradient_rgba(size);
		let expected = I420::from_rgba(&rgba, WIDTH * 4, size).unwrap();
		let frames: Vec<Frame> = (0..5)
			.map(|i| Frame::new(Surface::rgba(&rgba, size).unwrap(), at(i)))
			.collect();

		for frame in round_trip(&mut backend, &frames) {
			assert_close(&frame, &expected, 8);
		}
	}

	/// A VA-API decode handed out as DMA-BUFs is re-encoded without leaving the
	/// GPU, the transcode path, and still carries the picture.
	#[test]
	fn decoded_dmabufs_are_encoded_on_the_gpu() {
		let Some(mut backend) = backend() else { return };
		let size = Size::new(WIDTH, HEIGHT);
		let rgba = gradient_rgba(size);
		let expected = I420::from_rgba(&rgba, WIDTH * 4, size).unwrap();

		// A software-encoded source stream, decoded by VA-API into DMA-BUFs.
		let mut source = CrateEncoder::new(&Config {
			kind: EncodeKind::Software,
			..Config::new(WIDTH, HEIGHT, Rate::new(30, 1).unwrap())
		})
		.unwrap();
		let mut hardware = decoder(NAME, Output::Native);
		let mut frames = Vec::new();
		for i in 0..5 {
			if i == 0 {
				source.cut().unwrap();
			}
			let frame = Frame::new(Surface::rgba(&rgba, size).unwrap(), at(i));
			for encoded in source.encode(&frame).unwrap() {
				frames.extend(hardware.decode(encoded.payload, encoded.timestamp, i == 0).unwrap());
			}
		}
		frames.extend(hardware.flush().unwrap());
		assert!(
			frames.iter().all(|frame| matches!(frame.surface, Surface::DmaBuf(_))),
			"the decoder handed out CPU frames, so nothing here tests the GPU path"
		);

		let decoded = round_trip(&mut backend, &frames);
		assert!(
			backend.gpu_frames > 0 && backend.refused.is_empty(),
			"the encoder fell back to the CPU"
		);
		for frame in decoded {
			// Two generations of lossy coding.
			assert_close(&frame, &expected, 10);
		}
	}

	/// A packed RGB DMA-BUF, what a PipeWire screen capture delivers, is scaled
	/// and converted on the GPU and encoded from there.
	#[test]
	fn an_rgb_dmabuf_is_scaled_and_encoded_on_the_gpu() {
		let Some(mut backend) = backend() else { return };
		let source_size = Size::new(WIDTH * 2, HEIGHT * 2);
		let rgba = gradient_rgba(source_size);
		let Some(buffer) = vaapi::testing::bgrx_dmabuf(&rgba, source_size) else {
			eprintln!("skipping: no VA-API device to allocate a BGRX surface on");
			return;
		};
		let frame = Frame::new(Surface::DmaBuf(buffer), at(0));

		let scaled = frame
			.resize(Size::new(WIDTH, HEIGHT), &crate::resize::Config::default())
			.expect("resize");
		let Surface::DmaBuf(ref buffer) = scaled.surface else {
			panic!("the resize left the GPU");
		};
		assert_eq!(buffer.format(), crate::DrmFormat::NV12);
		assert_eq!((buffer.width(), buffer.height()), (WIDTH, HEIGHT));

		let expected = I420::from_rgba(&rgba, source_size.width * 4, source_size)
			.unwrap()
			.resize(Size::new(WIDTH, HEIGHT))
			.unwrap();
		// The GPU read-back of the scaled buffer, before any coding.
		let read_back = scaled.surface.to_i420().expect("read the scaled buffer back");
		assert_eq!(read_back.color(), Some(crate::Color::infer(source_size)));

		let decoded = round_trip(&mut backend, std::slice::from_ref(&scaled));
		assert!(
			backend.gpu_frames > 0 && backend.refused.is_empty(),
			"the encoder fell back to the CPU"
		);
		assert_close(&decoded[0], &expected, 10);
	}

	/// The encoder takes a packed RGB DMA-BUF at its own size directly.
	#[test]
	fn an_rgb_dmabuf_at_the_encoder_size_is_converted_on_the_gpu() {
		let Some(mut backend) = backend() else { return };
		let size = Size::new(WIDTH, HEIGHT);
		let rgba = gradient_rgba(size);
		let Some(buffer) = vaapi::testing::bgrx_dmabuf(&rgba, size) else {
			eprintln!("skipping: no VA-API device to allocate a BGRX surface on");
			return;
		};
		let expected = I420::from_rgba(&rgba, WIDTH * 4, size).unwrap();

		let decoded = round_trip(&mut backend, &[Frame::new(Surface::DmaBuf(buffer), at(0))]);
		assert!(
			backend.gpu_frames > 0 && backend.refused.is_empty(),
			"the encoder fell back to the CPU"
		);
		assert_close(&decoded[0], &expected, 10);
	}

	/// A bitrate change reaches the encoder instead of being refused.
	#[test]
	fn the_bitrate_can_change_mid_stream() {
		let Some(mut backend) = backend() else { return };
		let size = Size::new(WIDTH, HEIGHT);
		let rgba = gradient_rgba(size);
		let frames: Vec<Frame> = (0..6)
			.map(|i| Frame::new(Surface::rgba(&rgba, size).unwrap(), at(i)))
			.collect();
		for (index, frame) in frames.iter().enumerate() {
			if index == 3 {
				backend.set_bitrate(250_000).expect("set the bitrate");
			}
			assert!(!backend.encode(frame, index == 0).expect("encode").is_empty());
		}
		assert_eq!(backend.encoder.config().bitrate, 250_000);
	}

	/// A buffer the driver cannot import is encoded through the CPU, and its
	/// layout is not tried on the GPU again.
	#[test]
	fn a_refused_dmabuf_is_encoded_through_the_cpu() {
		let Some(mut backend) = backend() else { return };
		let size = Size::new(WIDTH, HEIGHT);
		let rgba = gradient_rgba(size);
		let expected = I420::from_rgba(&rgba, WIDTH * 4, size).unwrap();
		let frames: Vec<Frame> = (0..3)
			.map(|i| {
				let buffer = vaapi::testing::unimportable_dmabuf(expected.clone());
				Frame::new(Surface::DmaBuf(buffer), at(i))
			})
			.collect();

		let decoded = round_trip(&mut backend, &frames);
		assert_eq!(backend.gpu_frames, 0);
		assert_eq!(backend.refused.len(), 1, "the refused layout is remembered once");
		for frame in decoded {
			assert_close(&frame, &expected, 8);
		}
	}

	/// A buffer the video processor cannot import is resized on the CPU, the
	/// same as without VA-API.
	#[test]
	fn a_refused_dmabuf_is_resized_on_the_cpu() {
		let size = Size::new(WIDTH, HEIGHT);
		let expected = I420::from_rgba(&gradient_rgba(size), WIDTH * 4, size).unwrap();
		let buffer = vaapi::testing::unimportable_dmabuf(expected.clone());
		let half = Size::new(WIDTH / 2, HEIGHT / 2);
		let scaled = Frame::new(Surface::DmaBuf(buffer), at(0))
			.resize(half, &crate::resize::Config::default())
			.expect("resize");
		let Surface::I420(ref pixels) = scaled.surface else {
			panic!("a refused buffer came back on the GPU");
		};
		let reference = expected.resize(half).unwrap();
		assert_eq!(mae(pixels.y(), reference.y()), 0, "the CPU path is the CPU resize");
	}

	/// The SPS names the color space the stream was configured with, read back
	/// out of the bitstream.
	#[test]
	fn the_sps_declares_the_color_space() {
		use super::super::test_util::{BT601_DESCRIBED, BT709_DESCRIBED, declared_color};
		use crate::Color;

		// Opposite of the space `resolved_color` would infer from the size, so a
		// backend that ignores `Config::color` cannot pass.
		for (size, color, described) in [
			(Size::new(640, 480), Color::Bt709Limited, BT709_DESCRIBED),
			(Size::new(1920, 1080), Color::Bt601Limited, BT601_DESCRIBED),
		] {
			let config = Config {
				kind: EncodeKind::Named(NAME.into()),
				color: Some(color),
				..Config::new(size.width, size.height, Rate::new(30, 1).unwrap())
			};
			let Ok(mut backend) = Vaapi::new(&config) else {
				eprintln!("skipping: no VA-API H.264 encoder");
				return;
			};
			let rgba = [255u8, 0, 0, 255].repeat(size.pixels() as usize);
			let frame = Frame::new(Surface::rgba(&rgba, size).unwrap(), at(0));
			let encoded = backend.encode(&frame, true).unwrap();
			let annexb = &encoded.first().expect("a keyframe").payload;
			assert_eq!(declared_color(annexb), Some(described), "{size} SPS color description");
		}
	}
}
