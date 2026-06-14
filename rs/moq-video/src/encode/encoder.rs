//! H.264 encoder front end.
//!
//! Accepts raw RGBA frames, converts them to I420, and delegates the actual
//! encode to a [`Backend`](super::backend::Backend). The resulting Annex-B
//! H.264 packets are ready for `moq_mux::codec::h264::Import`.

use bytes::Bytes;

use super::backend::{self, Backend};
use crate::Error;
use crate::frame::{Frame, I420};

/// Which encoder implementation to use. `#[non_exhaustive]` so new selection
/// strategies can be added without breaking external `match`es.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform hardware encoder, fall back to software.
	#[default]
	Auto,
	/// Hardware only; error if none is available.
	Hardware,
	/// Software (openh264) only.
	Software,
	/// A specific backend by name, e.g. `"videotoolbox"`, `"nvenc"`, `"vaapi"`,
	/// `"openh264"`.
	Named(String),
}

/// Encoder configuration. `width` / `height` / `framerate` are the encoded
/// output; input frames must already be at this resolution.
///
/// `#[non_exhaustive]`: build via [`Config::new`] and set the optional fields,
/// so future knobs don't break callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	pub width: u32,
	pub height: u32,
	pub framerate: u32,
	/// Target bitrate in bits per second. `None` derives a sane default
	/// from resolution and framerate (~0.07 bits per pixel per second).
	pub bitrate: Option<u64>,
	/// Keyframe interval in frames. Subscribers joining mid-stream wait at
	/// most this many frames before they can start decoding.
	pub gop: u32,
	pub kind: Kind,
}

impl Config {
	pub fn new(width: u32, height: u32, framerate: u32) -> Self {
		Self {
			width,
			height,
			framerate,
			bitrate: None,
			// ~2 seconds at the configured framerate.
			gop: framerate.saturating_mul(2).max(1),
			kind: Kind::Auto,
		}
	}

	/// Resolved bitrate: explicit override, or a pixels-per-second estimate.
	pub(crate) fn resolved_bitrate(&self) -> u64 {
		self.bitrate.unwrap_or_else(|| {
			let pixels = self.width as u64 * self.height as u64;
			// 0.07 bits per pixel per second matches the JS publisher's
			// default and lands ~4.4 Mbps for 1080p30.
			((pixels * self.framerate as u64) as f64 * 0.07) as u64
		})
	}
}

/// H.264 encoder. Build one with [`Encoder::new`], feed it raw RGBA frames
/// via [`encode_rgba`](Self::encode_rgba), and publish the resulting Annex-B
/// packets through [`Producer`](super::Producer).
pub struct Encoder {
	backend: Box<dyn Backend>,
	width: u32,
	height: u32,
}

impl Encoder {
	pub fn new(config: &Config) -> Result<Self, Error> {
		// Validate at the construction boundary so both entry points (the
		// capture loop and a bring-your-own-frames caller) reject a zero
		// framerate, which would produce a degenerate codec time base.
		if config.framerate == 0 {
			return Err(Error::InvalidFramerate(0));
		}
		if config.width == 0 || config.height == 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"encoder dimensions must be non-zero (got {}x{})",
				config.width,
				config.height
			)));
		}
		// I420 chroma is subsampled 2x2, so the encoded resolution must be even.
		if config.width % 2 != 0 || config.height % 2 != 0 {
			return Err(Error::Codec(anyhow::anyhow!(
				"encoder dimensions must be even (got {}x{})",
				config.width,
				config.height
			)));
		}

		let backend = backend::open(config)?;
		Ok(Self {
			backend,
			width: config.width,
			height: config.height,
		})
	}

	/// The encoder name in use, e.g. `"videotoolbox"`.
	pub fn name(&self) -> &str {
		self.backend.name()
	}

	/// Encode one tightly-packed RGBA frame (`width * height * 4` bytes),
	/// returning zero or more Annex-B H.264 packets. Set `keyframe` to force an
	/// IDR (e.g. on resume so a re-subscribing viewer can start decoding at
	/// once). The frame must already be at the encoder's resolution.
	pub fn encode_rgba(&mut self, rgba: &[u8], width: u32, height: u32, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		// Validate geometry up front: the encoder resolution is even (checked in
		// `new`), so a matching frame is even too, and the conversion can't fail
		// on odd dimensions.
		if width != self.width || height != self.height {
			return Err(Error::Codec(anyhow::anyhow!(
				"frame {width}x{height} does not match encoder {}x{}",
				self.width,
				self.height
			)));
		}
		let expected = (width as usize)
			.checked_mul(height as usize)
			.and_then(|pixels| pixels.checked_mul(4))
			.ok_or_else(|| Error::Codec(anyhow::anyhow!("RGBA size overflow for {width}x{height}")))?;
		if rgba.len() < expected {
			return Err(Error::Codec(anyhow::anyhow!(
				"RGBA buffer too small: {} < {expected} for {width}x{height}",
				rgba.len()
			)));
		}

		let frame = Frame::I420(I420::from_rgba(rgba, width, height)?);
		self.encode(&frame, keyframe)
	}

	/// Encode a captured [`Frame`] (a GPU surface or CPU I420). The frame must
	/// already be at the encoder's resolution.
	pub(crate) fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		if frame.width() != self.width || frame.height() != self.height {
			return Err(Error::Codec(anyhow::anyhow!(
				"frame {}x{} does not match encoder {}x{}",
				frame.width(),
				frame.height(),
				self.width,
				self.height
			)));
		}
		self.backend.encode(frame, keyframe)
	}

	/// Flush the encoder, returning any buffered packets.
	pub fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		self.backend.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A mid-gray RGBA frame: encodable without a camera.
	fn gray_rgba(width: u32, height: u32) -> Vec<u8> {
		vec![0x80u8; width as usize * height as usize * 4]
	}

	#[test]
	fn software_encoder_emits_annexb() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).expect("openh264 is vendored, always available");
		assert_eq!(encoder.name(), "openh264");

		let frame = gray_rgba(320, 240);
		let mut packets = Vec::new();
		for i in 0..30 {
			packets.extend(encoder.encode_rgba(&frame, 320, 240, i == 0).unwrap());
		}
		packets.extend(encoder.finish().unwrap());

		assert!(!packets.is_empty(), "encoder produced no packets");

		// The first packet must start with an Annex-B start code so the avc3
		// importer can find the inline SPS/PPS.
		let first = &packets[0];
		let has_start_code = first.starts_with(&[0, 0, 0, 1]) || first.starts_with(&[0, 0, 1]);
		assert!(
			has_start_code,
			"first packet is not Annex-B: {:02x?}",
			&first[..first.len().min(8)]
		);
	}

	#[test]
	fn encode_rgba_emits_annexb() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		let rgba = gray_rgba(320, 240);
		let mut packets = encoder.encode_rgba(&rgba, 320, 240, true).unwrap();
		packets.extend(encoder.finish().unwrap());
		assert!(!packets.is_empty());
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	#[test]
	fn encode_rgba_rejects_short_buffer() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();
		// Far smaller than 320*240*4: must error, not panic on conversion.
		assert!(matches!(
			encoder.encode_rgba(&[0u8; 16], 320, 240, false),
			Err(Error::Codec(_))
		));
	}

	#[test]
	fn encode_rgba_rejects_dimension_mismatch() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = gray_rgba(640, 480);
		assert!(matches!(
			encoder.encode_rgba(&rgba, 640, 480, false),
			Err(Error::Codec(_))
		));
	}

	#[test]
	fn new_rejects_zero_framerate() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 0)
		};
		assert!(matches!(Encoder::new(&config), Err(Error::InvalidFramerate(0))));
	}

	#[test]
	fn unknown_named_encoder_errors() {
		let config = Config {
			kind: Kind::Named("definitely_not_a_codec".into()),
			..Config::new(320, 240, 30)
		};
		assert!(matches!(Encoder::new(&config), Err(Error::NoEncoder(_))));
	}

	/// Exercises the hand-rolled VideoToolbox backend end to end on macOS:
	/// synthetic frames through the real `VTCompressionSession`, asserting the
	/// AVCC -> Annex-B conversion produces a self-contained IDR (SPS+PPS+slice).
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_emits_annexb_keyframe() {
		let config = Config {
			kind: Kind::Named("videotoolbox".into()),
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).expect("videotoolbox is available on macOS");
		assert_eq!(encoder.name(), "videotoolbox");

		let frame = gray_rgba(320, 240);
		let mut packets = Vec::new();
		for i in 0..10 {
			packets.extend(encoder.encode_rgba(&frame, 320, 240, i == 0).unwrap());
		}
		packets.extend(encoder.finish().unwrap());

		assert!(!packets.is_empty(), "encoder produced no packets");
		let first = &packets[0];
		assert!(
			first.starts_with(&[0, 0, 0, 1]) || first.starts_with(&[0, 0, 1]),
			"first packet is not Annex-B"
		);

		// The first access unit must be a self-contained IDR: SPS (7), PPS (8),
		// IDR slice (5), all spliced in-band by the AVCC -> Annex-B conversion.
		let types = nal_types(first);
		assert!(types.contains(&7), "no SPS in first packet: {types:?}");
		assert!(types.contains(&8), "no PPS in first packet: {types:?}");
		assert!(types.contains(&5), "first packet is not an IDR: {types:?}");
	}

	/// Feed a GPU surface (NV12 `CVPixelBuffer`) straight into VideoToolbox:
	/// the zero-copy capture -> encode path, no I420 round-trip.
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_encodes_surface_zero_copy() {
		let config = Config {
			kind: Kind::Named("videotoolbox".into()),
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		let mut packets = Vec::new();
		for i in 0..10 {
			let frame = Frame::Surface(nv12_surface(320, 240));
			packets.extend(encoder.encode(&frame, i == 0).unwrap());
		}
		packets.extend(encoder.finish().unwrap());

		assert!(!packets.is_empty());
		let types = nal_types(&packets[0]);
		assert!(
			types.contains(&7) && types.contains(&8) && types.contains(&5),
			"no IDR: {types:?}"
		);
	}

	/// A software encoder must download a GPU surface to I420 first. Exercises
	/// the NV12 -> I420 fallback path.
	#[cfg(target_os = "macos")]
	#[test]
	fn openh264_downloads_surface() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		let frame = Frame::Surface(nv12_surface(320, 240));
		let mut packets = encoder.encode(&frame, true).unwrap();
		packets.extend(encoder.finish().unwrap());

		assert!(!packets.is_empty());
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	/// A mid-gray NV12 `CVPixelBuffer`, the format AVFoundation/ScreenCaptureKit
	/// hand us. Y and interleaved UV planes filled with 128.
	#[cfg(target_os = "macos")]
	fn nv12_surface(width: u32, height: u32) -> crate::frame::macos::Surface {
		use std::ptr::{self, NonNull};

		use objc2_core_foundation::CFRetained;
		use objc2_core_video::{
			CVPixelBuffer, CVPixelBufferCreate, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
			CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
			kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
		};

		let mut raw: *mut CVPixelBuffer = ptr::null_mut();
		let status = unsafe {
			CVPixelBufferCreate(
				None,
				width as usize,
				height as usize,
				kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
				None,
				NonNull::new(&mut raw).unwrap(),
			)
		};
		assert_eq!(status, 0, "CVPixelBufferCreate failed");
		let buffer = unsafe { CFRetained::from_raw(NonNull::new(raw).unwrap()) };

		let flags = CVPixelBufferLockFlags(0);
		assert_eq!(unsafe { CVPixelBufferLockBaseAddress(&buffer, flags) }, 0);
		for (plane, rows) in [(0usize, height as usize), (1usize, height as usize / 2)] {
			let base = CVPixelBufferGetBaseAddressOfPlane(&buffer, plane) as *mut u8;
			let stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, plane);
			unsafe { ptr::write_bytes(base, 128, stride * rows) };
		}
		unsafe { CVPixelBufferUnlockBaseAddress(&buffer, flags) };

		crate::frame::macos::Surface::new(buffer, width, height)
	}

	/// NAL unit types in an Annex-B buffer, found via 3-byte start codes.
	#[cfg(target_os = "macos")]
	fn nal_types(annexb: &[u8]) -> Vec<u8> {
		let mut types = Vec::new();
		let mut i = 0;
		while i + 3 < annexb.len() {
			if annexb[i..i + 3] == [0, 0, 1] {
				types.push(annexb[i + 3] & 0x1f);
				i += 3;
			} else {
				i += 1;
			}
		}
		types
	}

	#[test]
	fn default_bitrate_scales_with_resolution() {
		let small = Config::new(320, 240, 30).resolved_bitrate();
		let large = Config::new(1920, 1080, 30).resolved_bitrate();
		assert!(large > small);
		assert!(small > 0);
	}
}
