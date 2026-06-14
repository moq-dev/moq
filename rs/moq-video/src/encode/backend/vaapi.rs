//! Intel/AMD VAAPI hardware backend via `cros-codecs` (Linux, `vaapi` feature).
//!
//! VAAPI is a *stateless* encode API: the hardware does the slice coding, but the
//! application builds the H.264 bitstream (SPS/PPS, DPB, ref lists). cros-codecs
//! is that bitstream layer; we drive its H.264 VAAPI encoder. Output is an
//! Annex-B elementary stream with in-band SPS/PPS, matching avc3 mode.
//!
//! We consume [discord/cros-codecs](https://github.com/discord/cros-codecs): the
//! maintained fork that builds against modern libva (cros-libva 0.0.13; the
//! published cros-codecs pins the broken 0.0.12) and hardens the H.264 encoder
//! (packed SPS/PPS + slice headers, frame_num, rate control). libva is dlopen'd
//! at runtime (`vaapi_dlopen`), so this links without libva and the binary loads
//! on machines without it, falling back to software (see [`backend::open`]).
//!
//! Input path: the encoder wants an **NV12** VA surface, but our captures hand us
//! CPU I420 (webcams deliver YUYV/MJPEG, decoded to I420; the camera rarely
//! offers NV12 to import zero-copy). So each frame uploads I420 into a pooled VA
//! surface as NV12, then encodes the surface. A zero-copy dmabuf path (import a
//! captured NV12 dmabuf straight into a VA surface) is a follow-up for the rare
//! NV12-capable V4L2 source.
//!
//! NOT YET VALIDATED ON HARDWARE. Compiles on Linux with libva headers, but needs
//! a Linux + Intel/AMD GPU to confirm: (1) the `low_power` entrypoint (recent
//! Intel iHD often requires the low-power encode entrypoint, AMD the full one; we
//! request full and let `Kind::Auto` fall back); (2) the NV12 surface upload
//! (plane pitch/offset handling) round-trips correctly.

use std::borrow::Borrow;

use bytes::Bytes;
use cros_codecs::backend::vaapi::surface_pool::{PooledVaSurface, VaSurfacePool};
use cros_codecs::decoder::FramePool;
use cros_codecs::encoder::h264::EncoderConfig as H264Config;
use cros_codecs::encoder::stateless::h264::StatelessEncoder;
use cros_codecs::encoder::{FrameMetadata, RateControl, Tunings, VideoEncoder};
use cros_codecs::libva::{self, Display, Image, Surface, UsageHint};
use cros_codecs::{BlockingMode, Fourcc, FrameLayout, PlaneLayout, Resolution};

use super::super::encoder::Config;
use super::Backend;
use crate::Error;
use crate::frame::{Frame, I420};

pub(crate) const NAME: &str = "vaapi";

/// Input surfaces to keep in the pool; a small ring covers frames in flight
/// under `BlockingMode::Blocking` (reference surfaces are managed separately by
/// the encoder backend).
const SURFACE_COUNT: usize = 8;

pub(crate) struct Vaapi {
	encoder: Box<dyn VideoEncoder<PooledVaSurface<()>>>,
	pool: VaSurfacePool<()>,
	/// NV12 image format for surface upload, queried once at open.
	nv12_format: libva::VAImageFormat,
	/// Logical NV12 layout handed to the encoder with each frame.
	layout: FrameLayout,
	timestamp: u64,
}

// The encoder, surface pool, and VA `Display` are `!Send` (`Rc` internally) but
// used only from the single capture/encode thread (see `publish_capture`).
unsafe impl Send for Vaapi {}

impl Vaapi {
	pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
		let display = Display::open().ok_or_else(|| Error::Codec(anyhow::anyhow!("open VAAPI display")))?;

		let size = Resolution {
			width: config.width,
			height: config.height,
		};
		let h264 = H264Config {
			resolution: size,
			initial_tunings: Tunings {
				rate_control: RateControl::ConstantBitrate(config.resolved_bitrate()),
				framerate: config.framerate,
				..Default::default()
			},
			..Default::default()
		};

		let encoder = StatelessEncoder::new_native_vaapi(
			display.clone(),
			h264,
			Fourcc::from(b"NV12"),
			size,
			false, // low_power: recent Intel may need true; validate on hardware.
			BlockingMode::Blocking,
		)
		.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI encoder init: {e:?}")))?;

		let mut pool = VaSurfacePool::new(
			display.clone(),
			libva::VA_RT_FORMAT_YUV420,
			Some(UsageHint::USAGE_HINT_ENCODER),
			size,
		);
		pool.add_frames(vec![(); SURFACE_COUNT])
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI surface pool: {e}")))?;

		let nv12_format = display
			.query_image_formats()
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI query image formats: {e}")))?
			.into_iter()
			.find(|f| f.fourcc == libva::VA_FOURCC_NV12)
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("VAAPI driver has no NV12 image format")))?;

		// Tightly-packed NV12: Y plane (w*h) then interleaved UV (w*h/2).
		let layout = FrameLayout {
			format: (Fourcc::from(b"NV12"), 0),
			size,
			planes: vec![
				PlaneLayout {
					buffer_index: 0,
					offset: 0,
					stride: config.width as usize,
				},
				PlaneLayout {
					buffer_index: 0,
					offset: (config.width * config.height) as usize,
					stride: config.width as usize,
				},
			],
		};

		tracing::info!(
			encoder = NAME,
			width = config.width,
			height = config.height,
			"opened H.264 encoder"
		);
		Ok(Box::new(Self {
			encoder: Box::new(encoder) as Box<dyn VideoEncoder<PooledVaSurface<()>>>,
			pool,
			nv12_format,
			layout,
			timestamp: 0,
		}))
	}

	/// Upload tightly-packed I420 into a VA surface as NV12: copy Y as-is, then
	/// interleave U and V into the chroma plane, honoring the surface's pitches.
	fn upload(&self, surface: &Surface<()>, i420: &I420) -> Result<(), Error> {
		let mut image = Image::create_from(surface, self.nv12_format, surface.size(), surface.size())
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI map surface: {e}")))?;

		let va = *image.image();
		let dst = image.as_mut();

		let (w, h) = (i420.width as usize, i420.height as usize);
		let (cw, ch) = (w / 2, h / 2);

		let (y_off, y_pitch) = (va.offsets[0] as usize, va.pitches[0] as usize);
		let y = i420.y();
		for row in 0..h {
			let d = y_off + row * y_pitch;
			dst[d..d + w].copy_from_slice(&y[row * w..row * w + w]);
		}

		let (uv_off, uv_pitch) = (va.offsets[1] as usize, va.pitches[1] as usize);
		let (u, v) = (i420.u(), i420.v());
		for row in 0..ch {
			let base = uv_off + row * uv_pitch;
			for col in 0..cw {
				dst[base + col * 2] = u[row * cw + col];
				dst[base + col * 2 + 1] = v[row * cw + col];
			}
		}

		// Drop the image to flush the upload, then make sure it lands before the
		// encoder reads the surface.
		drop(image);
		surface
			.sync()
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI surface sync: {e}")))
	}

	/// Drain any ready coded bitstream buffers into Annex-B packets.
	fn drain_ready(&mut self) -> Result<Vec<Bytes>, Error> {
		let mut out = Vec::new();
		while let Some(coded) = self
			.encoder
			.poll()
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI poll: {e:?}")))?
		{
			out.push(Bytes::from(coded.bitstream));
		}
		Ok(out)
	}
}

impl Backend for Vaapi {
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		let i420 = frame.to_i420()?;

		let surface = self
			.pool
			.get_surface()
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("VAAPI surface pool exhausted")))?;
		self.upload(surface.borrow(), &i420)?;

		// Force an IDR (not just any keyframe) so a re-subscribing viewer gets a
		// clean random-access point: references cleared and in-band SPS/PPS.
		let meta = FrameMetadata {
			timestamp: self.timestamp,
			layout: self.layout.clone(),
			force_keyframe: keyframe,
			force_idr: keyframe,
		};
		self.timestamp += 1;

		self.encoder
			.encode(meta, surface)
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI encode: {e:?}")))?;
		self.drain_ready()
	}

	fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		self.encoder
			.drain()
			.map_err(|e| Error::Codec(anyhow::anyhow!("VAAPI drain: {e:?}")))?;
		self.drain_ready()
	}

	fn name(&self) -> &str {
		NAME
	}
}
