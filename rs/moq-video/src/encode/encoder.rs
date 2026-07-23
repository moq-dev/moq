//! Video encoder front end.
//!
//! Accepts raw RGBA frames, converts them to I420, and delegates the actual
//! encode to a [`Backend`](super::backend::Backend). The resulting packets are
//! Annex-B in the framing the catalog importer for [`Config::codec`] expects:
//! H.264 (`moq_mux::codec::h264`) or H.265 (`moq_mux::codec::h265`).

use bytes::Bytes;

use super::backend::{self, Backend};
use crate::frame::{I420, Surface};
use crate::{Error, Size};

/// Output video codec. `#[non_exhaustive]` so new codecs can be added without
/// breaking external `match`es.
///
/// Not every codec has a backend on every platform: H.265 is hardware-only
/// (VideoToolbox on macOS today). Building an [`Encoder`] returns
/// [`Error::NoEncoder`](crate::Error::NoEncoder) when nothing can encode the
/// requested codec on this machine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
	/// H.264 / AVC, Annex-B with in-band SPS/PPS (the "avc3" shape). The widest
	/// support and the default.
	#[default]
	H264,
	/// H.265 / HEVC, Annex-B with in-band VPS/SPS/PPS (the "hev1" shape).
	H265,
}

/// Which encoder implementation to use. `#[non_exhaustive]` so new selection
/// strategies can be added without breaking external `match`es.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform hardware encoder, falling back to the openh264 software
	/// encoder when none is available.
	#[default]
	Auto,
	/// Hardware only; error if none is available.
	Hardware,
	/// Software only (openh264 for H.264).
	Software,
	/// A specific backend by name, e.g. `"videotoolbox"`, `"nvenc"`, `"vaapi"`,
	/// or `"openh264"`.
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
	/// Output codec. Defaults to [`Codec::H264`].
	pub codec: Codec,
	pub kind: Kind,
}

impl Config {
	/// A config encoding `width` x `height` at `framerate`, with the default
	/// codec, GOP, and bitrate.
	pub fn new(width: u32, height: u32, framerate: u32) -> Self {
		Self {
			width,
			height,
			framerate,
			bitrate: None,
			// ~2 seconds at the configured framerate.
			gop: framerate.saturating_mul(2).max(1),
			codec: Codec::default(),
			kind: Kind::Auto,
		}
	}

	/// The encoded resolution.
	pub fn size(&self) -> Size {
		Size::new(self.width, self.height)
	}

	/// Resolved bitrate: explicit override, or a pixels-per-second estimate.
	pub(crate) fn resolved_bitrate(&self) -> u64 {
		self.bitrate.unwrap_or_else(|| {
			// 0.07 bits per pixel per second matches the JS publisher's
			// default and lands ~4.4 Mbps for 1080p30.
			((self.size().pixels() * self.framerate as u64) as f64 * 0.07) as u64
		})
	}
}

/// Video encoder. Build one with [`Encoder::new`], feed it raw RGBA frames via
/// [`encode_rgba`](Self::encode_rgba), and publish the resulting packets through
/// a [`Producer`](super::Producer) built for the same [`Codec`].
pub struct Encoder {
	backend: Box<dyn Backend>,
	codec: Codec,
	size: Size,
	bitrate: u64,
}

impl Encoder {
	/// Open an encoder for `config`.
	pub fn new(config: &Config) -> Result<Self, Error> {
		// Validate at the construction boundary so both entry points (the
		// capture loop and a bring-your-own-frames caller) reject a zero
		// framerate, which would produce a degenerate codec time base.
		if config.framerate == 0 {
			return Err(Error::InvalidFramerate(0));
		}
		// I420 chroma is subsampled 2x2, so the encoded resolution must be even.
		let size = config.size();
		size.validate("encoder")?;

		let backend = backend::open(config)?;
		Ok(Self {
			backend,
			codec: config.codec,
			size,
			bitrate: config.resolved_bitrate(),
		})
	}

	/// The encoder name in use, e.g. `"videotoolbox"`.
	pub fn name(&self) -> &str {
		self.backend.name()
	}

	/// The resolution this encoder emits, which every frame fed to it must match.
	pub fn size(&self) -> Size {
		self.size
	}

	/// The current target bitrate in bits per second: what
	/// [`Config::bitrate`] resolved to at open, or the last value
	/// [`set_bitrate`](Self::set_bitrate) accepted.
	pub fn bitrate(&self) -> u64 {
		self.bitrate
	}

	/// Retune the live encoder to `bitrate` bits per second, taking effect from
	/// roughly the next frame. No IDR is forced, so this is cheap enough to
	/// drive from a congestion controller: pair it with
	/// [`rate::Control`](super::rate::Control), which decides *when* the target
	/// is worth moving.
	///
	/// Setting the rate the encoder is already at does nothing and succeeds.
	///
	/// # Errors
	///
	/// Returns [`Error::BitrateUnsupported`] if this backend can't retune while
	/// running. That's not fatal: the encoder keeps running at its current rate,
	/// so a caller driving a control loop should stop adapting rather than stop
	/// encoding.
	pub fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error> {
		if bitrate == self.bitrate {
			return Ok(());
		}
		self.backend.set_bitrate(bitrate)?;
		// Only after the backend accepts it, so a failed set doesn't leave the
		// getter reporting a rate the encoder isn't using.
		self.bitrate = bitrate;
		Ok(())
	}

	/// The codec this encoder emits. A [`Producer`](super::Producer) must be
	/// built for the same codec to publish its packets.
	pub fn codec(&self) -> Codec {
		self.codec
	}

	/// Encode one tightly-packed RGBA frame of `size`, returning zero or more
	/// encoded packets in the codec's framing. Set `keyframe` to force an IDR
	/// (e.g. on resume so a re-subscribing viewer can start decoding at once).
	///
	/// `size` must equal the encoder's [`size`](Self::size), and `rgba` must hold
	/// exactly `width * height * 4` bytes with no row padding.
	pub fn encode_rgba(&mut self, rgba: &[u8], size: Size, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		// The encoder resolution is validated even and non-zero in `new`, so a
		// frame matching it is even too and the conversion below can't fail on odd
		// dimensions.
		self.check_frame(size, rgba.len(), size.pixels() as usize * 4, "RGBA")?;

		let frame = Surface::I420(I420::from_rgba(rgba, size.width * 4, size.width, size.height)?);
		self.encode(&frame, keyframe)
	}

	/// Encode one tightly-packed I420 frame of `size` (Y then U then V, no row
	/// padding, BT.601 limited range), returning zero or more encoded packets in
	/// the codec's framing. Set `keyframe` to force an IDR.
	///
	/// `size` must equal the encoder's [`size`](Self::size), and `i420` must hold
	/// exactly `width * height * 3 / 2` bytes.
	///
	/// The bring-your-own-I420 path, which copies the buffer to take ownership of
	/// it. A transcoder should prefer [`encode`](Self::encode): it takes a decoded
	/// frame directly and keeps a GPU one on the GPU, so it neither copies nor
	/// round-trips through system memory.
	pub fn encode_i420(&mut self, i420: &[u8], size: Size, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		self.check_frame(size, i420.len(), I420::len(size.width, size.height), "I420")?;

		let frame = Surface::I420(I420 {
			width: size.width,
			height: size.height,
			data: i420.to_vec(),
		});
		self.encode(&frame, keyframe)
	}

	/// Reject a frame the encoder can't encode: the wrong shape, or a buffer that
	/// doesn't hold exactly one frame of that shape.
	///
	/// Both halves are load-bearing. `size` catches a transposed frame, which the
	/// byte count alone cannot: 240x320 and 320x240 are the same number of bytes.
	/// The exact length then catches a buffer that doesn't match the shape it
	/// claims, rather than encoding its first frame's worth and ignoring the rest.
	fn check_frame(&self, size: Size, got: usize, expected: usize, what: &str) -> Result<(), Error> {
		if size != self.size {
			return Err(Error::Codec(anyhow::anyhow!(
				"frame {size} does not match encoder {}",
				self.size
			)));
		}
		if got != expected {
			return Err(Error::Codec(anyhow::anyhow!(
				"{what} buffer is {got} bytes, expected {expected} for {size}"
			)));
		}
		Ok(())
	}

	/// Encode a [`Surface`](crate::Surface), whether it came from capture or a
	/// decoder (the transcode input path).
	///
	/// A GPU surface feeds a hardware encoder on the same device directly
	/// (NVDEC -> NVENC never leaves the GPU, a `CVPixelBuffer` goes straight to
	/// VideoToolbox); anything else falls back to a CPU I420 upload. The surface
	/// must already be at the encoder's resolution: decode with
	/// [`decode::Config::resize`](crate::decode::Config), or scale first with
	/// [`decode::Frame::resize`](crate::decode::Frame::resize).
	pub fn encode(&mut self, frame: &Surface, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		let size = Size::new(frame.width(), frame.height());
		if size != self.size {
			return Err(Error::Codec(anyhow::anyhow!(
				"frame {size} does not match encoder {}",
				self.size
			)));
		}
		self.backend.encode(frame, keyframe)
	}

	/// Flush the encoder, returning any buffered packets.
	///
	/// Consumes the encoder: nothing can be encoded after a flush, so this is the
	/// last call rather than one leaving a drained encoder in your hands.
	pub fn finish(mut self) -> Result<Vec<Bytes>, Error> {
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
			packets.extend(encoder.encode_rgba(&frame, Size::new(320, 240), i == 0).unwrap());
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
		let mut packets = encoder.encode_rgba(&rgba, Size::new(320, 240), true).unwrap();
		packets.extend(encoder.finish().unwrap());
		assert!(!packets.is_empty());
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	#[test]
	fn encode_i420_emits_annexb() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		// A mid-gray I420 frame: flat 0x80 across all three planes.
		let data = vec![0x80u8; I420::len(320, 240)];
		let mut packets = encoder.encode_i420(&data, Size::new(320, 240), true).unwrap();
		packets.extend(encoder.finish().unwrap());
		assert!(!packets.is_empty());
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	/// A buffer that doesn't hold one whole frame of the declared size must error
	/// rather than reach a backend short.
	#[test]
	fn encode_i420_rejects_wrong_size() {
		let Ok(mut encoder) = Encoder::new(&Config::new(320, 240, 30)) else {
			return;
		};
		assert!(matches!(
			encoder.encode_i420(&[0u8; 16], Size::new(320, 240), false),
			Err(Error::Codec(_))
		));
	}

	#[test]
	fn encode_rgba_rejects_short_buffer() {
		let Ok(mut encoder) = Encoder::new(&Config::new(320, 240, 30)) else {
			return;
		};
		// Far smaller than 320*240*4: must error, not panic on conversion.
		assert!(matches!(
			encoder.encode_rgba(&[0u8; 16], Size::new(320, 240), false),
			Err(Error::Codec(_))
		));
	}

	/// A frame that isn't the encoder's size must error rather than encode its
	/// top-left corner.
	#[test]
	fn encode_rgba_rejects_dimension_mismatch() {
		let Ok(mut encoder) = Encoder::new(&Config::new(320, 240, 30)) else {
			return;
		};
		let rgba = gray_rgba(640, 480);
		assert!(matches!(
			encoder.encode_rgba(&rgba, Size::new(640, 480), false),
			Err(Error::Codec(_))
		));
	}

	/// The I420 counterpart: an oversized buffer is a mis-sized frame, not slack
	/// to truncate.
	#[test]
	fn encode_i420_rejects_oversized_buffer() {
		let Ok(mut encoder) = Encoder::new(&Config::new(320, 240, 30)) else {
			return;
		};
		let data = vec![0x80u8; I420::len(640, 480)];
		assert!(matches!(
			encoder.encode_i420(&data, Size::new(640, 480), false),
			Err(Error::Codec(_))
		));
	}

	/// A transposed frame is exactly why `size` is still a parameter: 240x320 and
	/// 320x240 hold the same number of bytes, so a length check alone would accept
	/// this and encode garbage. Both entry points must reject it.
	#[test]
	fn encode_rejects_transposed_frame() {
		let Ok(mut encoder) = Encoder::new(&Config::new(320, 240, 30)) else {
			return;
		};

		let rgba = gray_rgba(240, 320);
		assert_eq!(rgba.len(), gray_rgba(320, 240).len(), "the byte counts must collide");
		assert!(matches!(
			encoder.encode_rgba(&rgba, Size::new(240, 320), false),
			Err(Error::Codec(_))
		));

		let i420 = vec![0x80u8; I420::len(240, 320)];
		assert_eq!(i420.len(), I420::len(320, 240), "the byte counts must collide");
		assert!(matches!(
			encoder.encode_i420(&i420, Size::new(240, 320), false),
			Err(Error::Codec(_))
		));
	}

	#[test]
	fn new_rejects_zero_framerate() {
		// Framerate is validated before any backend opens, so this holds on every
		// platform regardless of which encoders are compiled in.
		let config = Config::new(320, 240, 0);
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
			packets.extend(encoder.encode_rgba(&frame, Size::new(320, 240), i == 0).unwrap());
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

	/// HEVC via VideoToolbox: synthetic frames through the real
	/// `VTCompressionSession` with `kCMVideoCodecType_HEVC`, asserting the
	/// HVCC -> Annex-B conversion produces a self-contained IRAP (VPS+SPS+PPS+IDR).
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_emits_annexb_keyframe_h265() {
		let config = Config {
			codec: Codec::H265,
			kind: Kind::Named("videotoolbox".into()),
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).expect("videotoolbox HEVC is available on macOS");
		assert_eq!(encoder.name(), "videotoolbox");
		assert_eq!(encoder.codec(), Codec::H265);

		let frame = gray_rgba(320, 240);
		let mut packets = Vec::new();
		for i in 0..10 {
			packets.extend(encoder.encode_rgba(&frame, Size::new(320, 240), i == 0).unwrap());
		}
		packets.extend(encoder.finish().unwrap());

		assert!(!packets.is_empty(), "encoder produced no packets");
		let first = &packets[0];
		assert!(
			first.starts_with(&[0, 0, 0, 1]) || first.starts_with(&[0, 0, 1]),
			"first packet is not Annex-B"
		);

		// The first access unit must be a self-contained IRAP: VPS (32), SPS (33),
		// PPS (34), and an IDR slice (16..=23), spliced in-band by the conversion.
		let types = hevc_nal_types(first);
		assert!(types.contains(&32), "no VPS in first packet: {types:?}");
		assert!(types.contains(&33), "no SPS in first packet: {types:?}");
		assert!(types.contains(&34), "no PPS in first packet: {types:?}");
		assert!(
			types.iter().any(|t| (16..=23).contains(t)),
			"first packet is not an IRAP: {types:?}"
		);
	}

	/// HEVC NAL unit types in an Annex-B buffer (type = `(byte >> 1) & 0x3f`).
	#[cfg(target_os = "macos")]
	fn hevc_nal_types(annexb: &[u8]) -> Vec<u8> {
		let mut types = Vec::new();
		let mut i = 0;
		while i + 3 < annexb.len() {
			if annexb[i..i + 3] == [0, 0, 1] {
				types.push((annexb[i + 3] >> 1) & 0x3f);
				i += 3;
			} else {
				i += 1;
			}
		}
		types
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
			let frame = Surface::PixelBuffer(nv12_surface(320, 240));
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

		let frame = Surface::PixelBuffer(nv12_surface(320, 240));
		let mut packets = encoder.encode(&frame, true).unwrap();
		packets.extend(encoder.finish().unwrap());

		assert!(!packets.is_empty());
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	/// A mid-gray NV12 `CVPixelBuffer`, the format AVFoundation/ScreenCaptureKit
	/// hand us. Y and interleaved UV planes filled with 128.
	#[cfg(target_os = "macos")]
	fn nv12_surface(width: u32, height: u32) -> crate::frame::macos::PixelBuffer {
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

		crate::frame::macos::PixelBuffer::new(buffer, width, height)
	}

	/// NAL unit types in an Annex-B buffer, found via 3-byte start codes (a
	/// 4-byte `00 00 00 01` code contains `00 00 01` too, so this catches both).
	#[cfg(any(target_os = "macos", target_os = "windows"))]
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

	/// CPU path: synthetic RGBA through the Media Foundation hardware encoder
	/// (I420 -> system-memory NV12 upload). Ignored: needs a hardware encoder MFT,
	/// which GPU-less CI runners lack. Run with `--ignored`.
	#[cfg(target_os = "windows")]
	#[test]
	#[ignore]
	fn mediafoundation_cpu_rgba() {
		let config = Config {
			kind: Kind::Named("mediafoundation".into()),
			..Config::new(640, 480, 30)
		};
		let mut encoder = Encoder::new(&config).expect("hardware H.264 encoder available");
		assert_eq!(encoder.name(), "mediafoundation");

		let frame = gray_rgba(640, 480);
		let mut packets = Vec::new();
		for i in 0..30 {
			packets.extend(encoder.encode_rgba(&frame, Size::new(640, 480), i == 0).unwrap());
		}
		packets.extend(encoder.finish().unwrap());

		assert!(!packets.is_empty(), "encoder produced no packets");
		let types = nal_types(&packets[0]);
		assert!(types.contains(&7), "no SPS in first packet: {types:?}");
		assert!(types.contains(&8), "no PPS in first packet: {types:?}");
		assert!(types.contains(&5), "first packet is not an IDR: {types:?}");
	}

	/// Full zero-copy path: real camera -> D3D11 NV12 texture -> hardware encoder
	/// via the DXGI device manager, no CPU round-trip. Ignored: needs a camera and
	/// a GPU. Run with `--ignored`.
	#[cfg(target_os = "windows")]
	#[tokio::test]
	#[ignore]
	async fn mediafoundation_camera_texture() {
		let mut camera = crate::capture::open(&crate::capture::Config::default())
			.await
			.expect("open default camera");
		let (w, h) = (camera.width(), camera.height());

		let config = Config {
			kind: Kind::Named("mediafoundation".into()),
			..Config::new(w, h, camera.framerate().unwrap_or(30))
		};
		let mut encoder = Encoder::new(&config).expect("hardware H.264 encoder available");

		let mut packets = Vec::new();
		let mut textures = 0;
		for i in 0..30 {
			let frame = camera.read().await.expect("frame, not end of stream");
			if matches!(frame, Surface::Texture(_)) {
				textures += 1;
			}
			packets.extend(encoder.encode(&frame, i == 0).unwrap());
		}
		packets.extend(encoder.finish().unwrap());

		// On a GPU this exercises the zero-copy texture path; the assert guards
		// against silently testing only the CPU fallback.
		assert!(textures > 0, "capture never produced a GPU texture");
		assert!(!packets.is_empty(), "encoder produced no packets");
		let types = nal_types(&packets[0]);
		assert!(
			types.contains(&7) && types.contains(&8) && types.contains(&5),
			"no IDR: {types:?}"
		);
	}

	/// The openh264 retune goes through the raw `set_option` FFI, so this covers
	/// both that the call is accepted and that the encoder keeps producing after
	/// it. A wrong option id or a bad `SBitrateInfo` layout would fail here.
	#[test]
	fn set_bitrate_retunes_software_encoder() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();
		let rgba = gray_rgba(320, 240);

		let opened = encoder.bitrate();
		assert_eq!(opened, config.resolved_bitrate());

		// Encode first: this is the live-retune path, once the encoder exists.
		encoder.encode_rgba(&rgba, Size::new(320, 240), true).unwrap();

		let halved = opened / 2;
		encoder.set_bitrate(halved).unwrap();
		assert_eq!(encoder.bitrate(), halved);

		// The retuned encoder must still emit a decodable keyframe, not wedge.
		let packets = encoder.encode_rgba(&rgba, Size::new(320, 240), true).unwrap();
		assert!(!packets.is_empty(), "encoder produced nothing after a retune");
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	/// Regression: openh264 creates its encoder lazily on the first frame and
	/// rejects `SetOption` with `cmInitExpected` until then. A retune before any
	/// frame must be deferred to the first encode, not reported as a failure.
	#[test]
	fn set_bitrate_before_the_first_frame_is_deferred() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		let halved = encoder.bitrate() / 2;
		encoder.set_bitrate(halved).expect("a retune before the first frame");
		assert_eq!(encoder.bitrate(), halved);

		// The deferred rate is applied during this encode, which must still work.
		let rgba = gray_rgba(320, 240);
		let packets = encoder.encode_rgba(&rgba, Size::new(320, 240), true).unwrap();
		assert!(!packets.is_empty());

		// And the encoder is live now, so a further retune takes the direct path.
		encoder.set_bitrate(halved / 2).unwrap();
		assert!(encoder.encode_rgba(&rgba, Size::new(320, 240), false).is_ok());
	}

	/// Setting the current rate must not reach the backend at all: the control
	/// loop is allowed to be chatty, and the encoder shouldn't pay for it.
	#[test]
	fn set_bitrate_to_current_is_a_noop() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		let opened = encoder.bitrate();
		encoder.set_bitrate(opened).unwrap();
		assert_eq!(encoder.bitrate(), opened);
	}

	#[test]
	fn default_bitrate_scales_with_resolution() {
		let small = Config::new(320, 240, 30).resolved_bitrate();
		let large = Config::new(1920, 1080, 30).resolved_bitrate();
		assert!(large > small);
		assert!(small > 0);
	}
}
