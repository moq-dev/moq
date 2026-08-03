//! Video encoder front end.
//!
//! Accepts raw [`Frame`]s and delegates the actual encode to a
//! [`Backend`](super::backend::Backend). The resulting frames carry Annex-B in
//! the framing the catalog importer for [`Config::codec`] expects: H.264
//! (`moq_mux::codec::h264`) or H.265 (`moq_mux::codec::h265`).

use super::Encoded;
use super::backend::{self, Backend};
use crate::{Color, Error, Frame, Size};

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
	/// The color space of the input frames, written into the bitstream's VUI so a
	/// decoder doesn't have to guess. `None` uses [`Color::infer`], which is both
	/// what the crate's own RGB conversions produce and what a player falls back
	/// to, so leaving it unset keeps the pixels and the label in agreement.
	///
	/// Set it only when feeding frames the crate did not convert and whose space
	/// you know from elsewhere.
	pub color: Option<Color>,
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
			color: None,
		}
	}

	/// The encoded resolution.
	pub fn size(&self) -> Size {
		Size::new(self.width, self.height)
	}

	/// Resolved input color space: explicit override, or the size-based guess
	/// every player makes for an untagged stream. Backends write this into the
	/// VUI; the crate's RGB conversions pick the same answer for the same size,
	/// so the samples match what the bitstream claims.
	pub(crate) fn resolved_color(&self) -> Color {
		self.color.unwrap_or_else(|| Color::infer(self.size()))
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

/// Video encoder. Build one with [`Encoder::new`], feed it raw [`Frame`]s via
/// [`encode`](Self::encode), and publish the resulting [`Encoded`] access units
/// through a [`Producer`](super::Producer) built for the same [`Codec`].
pub struct Encoder {
	backend: Box<dyn Backend>,
	codec: Codec,
	size: Size,
	bitrate: u64,
	/// What the backend wrote into the bitstream's VUI, kept so a frame declaring
	/// a different space is caught rather than silently mislabeled.
	color: Color,
	/// A keyframe asked for by [`Encoder::keyframe`], applied to the next frame.
	/// Held rather than applied immediately because the caller decides a group
	/// boundary before it has the frame that opens it.
	pending_keyframe: bool,
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
			color: config.resolved_color(),
			pending_keyframe: false,
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

	/// Ask for the next frame to be encoded as a keyframe (an IDR), on top of the
	/// ones [`Config::gop`] already inserts on its own.
	///
	/// Rarely needed: the encoder keys frames automatically, so reach for this only
	/// when something outside the encoder needs a decodable starting point at a
	/// specific frame. Opening a new group is the usual reason (a subscriber has to
	/// be able to start there); resuming after an idle gap is another.
	///
	/// The request waits for the next [`encode`](Self::encode) rather than applying
	/// at once, so it is safe to call before the frame exists. Calling it repeatedly
	/// before a frame arrives asks for one keyframe, not several.
	pub fn keyframe(&mut self) {
		self.pending_keyframe = true;
	}

	/// Encode one raw [`Frame`], whether it came from capture, a decoder (the
	/// transcode input path), or your own pixels via
	/// [`Surface::rgba`](crate::Surface::rgba).
	///
	/// Returns zero or more encoded access units, each carrying the timestamp of the
	/// raw frame it came from: a backend that buffers hands back an earlier frame's
	/// output, so the two don't always line up.
	///
	/// A GPU surface feeds a hardware encoder on the same device directly
	/// (NVDEC -> NVENC never leaves the GPU, a `CVPixelBuffer` goes straight to
	/// VideoToolbox); anything else falls back to a CPU I420 upload. The frame must
	/// already be at the encoder's resolution: decode with
	/// [`decode::Config::resize`](crate::decode::Config), or scale first with
	/// [`Frame::resize`](crate::Frame::resize).
	pub fn encode(&mut self, frame: &Frame) -> Result<Vec<Encoded>, Error> {
		// A transposed frame is why this compares the shape rather than a byte
		// count: 240x320 and 320x240 hold the same number of bytes.
		let size = frame.size();
		if size != self.size {
			return Err(Error::Codec(anyhow::anyhow!(
				"frame {size} does not match encoder {}",
				self.size
			)));
		}
		// The VUI is fixed when the session opens, so a frame in a different space
		// gets encoded under the wrong label. Reached by resizing across the
		// standard-definition boundary, where the pixels keep their space but the
		// encoder was sized into another one; `Config::color` is the way to pin it.
		//
		// Warn rather than reject: a live gateway transcoding a source whose VUI
		// disagrees with its resolution should keep serving a mislabeled stream
		// rather than drop it, and this is no worse than the untagged stream that
		// came before. Once, because it would otherwise fire every frame.
		if let Some(color) = frame.surface.color()
			&& color != self.color
		{
			static WARN_ONCE: std::sync::Once = std::sync::Once::new();
			WARN_ONCE.call_once(|| {
				tracing::warn!(
					frame = ?color,
					encoder = ?self.color,
					"frame color space differs from the one written into the bitstream; set encode::Config::color"
				);
			});
		}
		let encoded = self.backend.encode(frame, self.pending_keyframe)?;
		// Cleared only once the frame is through: a failed encode produced no
		// picture, so the request still belongs to whatever comes next rather than
		// being swallowed. The size check above returns early for the same reason.
		self.pending_keyframe = false;
		Ok(encoded)
	}

	/// Return every access unit the codec is still holding, leaving the encoder
	/// ready for the frames that follow. Each keeps the timestamp of the raw frame
	/// it was encoded from, so a drained tail stays in step with what came before
	/// it.
	///
	/// Reach for this at a boundary the output has to respect, which for a live
	/// broadcast is a group: a hardware codec that pipelines holds the last frames
	/// of a group past its end, and they would otherwise be published into the next
	/// group ahead of its keyframe, where a consumer joining there cannot decode
	/// them. Publishing frame-by-frame with no group structure needs none of this.
	///
	/// Not free: emptying the pipeline gives up the overlap between one frame's
	/// encode and the next frame's submission, so flush at boundaries rather than
	/// per frame.
	pub fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
		self.backend.flush()
	}

	/// Flush the encoder, returning any buffered frames. Each keeps the timestamp
	/// of the raw frame it was encoded from, so a drained tail stays in step with
	/// what was published before it.
	///
	/// Consumes the encoder: nothing can be encoded after a flush, so this is the
	/// last call rather than one leaving a drained encoder in your hands.
	pub fn finish(mut self) -> Result<Vec<Encoded>, Error> {
		self.backend.finish()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use crate::{I420, Surface};

	/// A mid-gray RGBA buffer: encodable without a camera.
	fn gray_rgba(width: u32, height: u32) -> Vec<u8> {
		vec![0x80u8; width as usize * height as usize * 4]
	}

	/// The `index`th frame of a mid-gray 30fps stream, so a round-tripped
	/// timestamp identifies the frame it came from.
	fn gray_frame(width: u32, height: u32, index: u64) -> Frame {
		let surface = Surface::rgba(&gray_rgba(width, height), Size::new(width, height)).unwrap();
		Frame::new(surface, at(index))
	}

	/// The presentation time of frame `index` at 30fps.
	fn at(index: u64) -> moq_net::Timestamp {
		moq_net::Timestamp::from_micros(index * 33_333).unwrap()
	}

	/// The payloads of some encoded frames, for the Annex-B assertions.
	fn payloads(frames: &[Encoded]) -> Vec<bytes::Bytes> {
		frames.iter().map(|f| f.payload.clone()).collect()
	}

	#[test]
	fn software_encoder_emits_annexb() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).expect("openh264 is vendored, always available");
		assert_eq!(encoder.name(), "openh264");

		let mut frames = Vec::new();
		for i in 0..30 {
			if i == 0 {
				encoder.keyframe();
			}
			frames.extend(encoder.encode(&gray_frame(320, 240, i)).unwrap());
		}
		frames.extend(encoder.finish().unwrap());

		assert!(!frames.is_empty(), "encoder produced no packets");

		// Every encoded frame carries the timestamp of the raw frame it came from,
		// so the stream stays in step even if a backend buffers.
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert!(
			micros.windows(2).all(|w| w[0] < w[1]),
			"encoded timestamps not strictly increasing: {micros:?}"
		);
		assert!(
			micros.iter().all(|&t| t % 33_333 == 0 && t < 30 * 33_333),
			"encoded timestamp outside the fed set: {micros:?}"
		);

		// The first packet must start with an Annex-B start code so the avc3
		// importer can find the inline SPS/PPS.
		let packets = payloads(&frames);
		let first = &packets[0];
		let has_start_code = first.starts_with(&[0, 0, 0, 1]) || first.starts_with(&[0, 0, 1]);
		assert!(
			has_start_code,
			"first packet is not Annex-B: {:02x?}",
			&first[..first.len().min(8)]
		);
	}

	/// The bring-your-own-pixels path: RGBA in through `Surface::rgba`, Annex-B out.
	#[test]
	fn encode_rgba_surface_emits_annexb() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		let mut frames = encoder.encode(&gray_frame(320, 240, 0)).unwrap();
		frames.extend(encoder.finish().unwrap());
		assert!(!frames.is_empty());
		let packets = payloads(&frames);
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	/// The same path starting from planar I420 the caller already has.
	#[test]
	fn encode_i420_surface_emits_annexb() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();

		// A mid-gray I420 frame: flat 0x80 across all three planes.
		let i420 = I420::new(320, 240, vec![0x80u8; I420::len(320, 240)]).unwrap();
		let frame = Frame::new(Surface::I420(i420), at(0));
		let mut frames = encoder.encode(&frame).unwrap();
		frames.extend(encoder.finish().unwrap());
		assert!(!frames.is_empty());
		let packets = payloads(&frames);
		assert!(packets[0].starts_with(&[0, 0, 0, 1]) || packets[0].starts_with(&[0, 0, 1]));
	}

	/// A frame that isn't the encoder's size must error rather than encode its
	/// top-left corner.
	#[test]
	fn encode_rejects_dimension_mismatch() {
		let Ok(mut encoder) = Encoder::new(&Config::new(320, 240, 30)) else {
			return;
		};
		assert!(matches!(encoder.encode(&gray_frame(640, 480, 0)), Err(Error::Codec(_))));
	}

	/// A transposed frame is why the encoder compares the shape rather than the
	/// byte count: 240x320 and 320x240 hold the same number of bytes, so a length
	/// check alone would accept this and encode garbage.
	#[test]
	fn encode_rejects_transposed_frame() {
		let Ok(mut encoder) = Encoder::new(&Config::new(320, 240, 30)) else {
			return;
		};

		let transposed = gray_frame(240, 320, 0);
		assert_eq!(
			gray_rgba(240, 320).len(),
			gray_rgba(320, 240).len(),
			"the byte counts must collide"
		);
		assert!(matches!(encoder.encode(&transposed), Err(Error::Codec(_))));
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

		let mut frames = Vec::new();
		for i in 0..10 {
			if i == 0 {
				encoder.keyframe();
			}
			frames.extend(encoder.encode(&gray_frame(320, 240, i)).unwrap());
		}
		frames.extend(encoder.finish().unwrap());

		assert!(!frames.is_empty(), "encoder produced no packets");
		// VideoToolbox completes each frame before returning, so the timestamps come
		// back one per input frame, in order.
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert!(
			micros.windows(2).all(|w| w[0] < w[1]),
			"encoded timestamps not strictly increasing: {micros:?}"
		);

		let packets = payloads(&frames);
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

		let mut frames = Vec::new();
		for i in 0..10 {
			if i == 0 {
				encoder.keyframe();
			}
			frames.extend(encoder.encode(&gray_frame(320, 240, i)).unwrap());
		}
		frames.extend(encoder.finish().unwrap());

		assert!(!frames.is_empty(), "encoder produced no packets");
		let packets = payloads(&frames);
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

		let mut frames = Vec::new();
		for i in 0..10 {
			if i == 0 {
				encoder.keyframe();
			}
			let frame = Frame::new(Surface::PixelBuffer(nv12_surface(320, 240)), at(i));
			frames.extend(encoder.encode(&frame).unwrap());
		}
		frames.extend(encoder.finish().unwrap());

		assert!(!frames.is_empty());
		let packets = payloads(&frames);
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

		encoder.keyframe();
		let frame = Frame::new(Surface::PixelBuffer(nv12_surface(320, 240)), at(0));
		let mut frames = encoder.encode(&frame).unwrap();
		frames.extend(encoder.finish().unwrap());

		assert!(!frames.is_empty());
		let packets = payloads(&frames);
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

		let mut frames = Vec::new();
		for i in 0..30 {
			if i == 0 {
				encoder.keyframe();
			}
			frames.extend(encoder.encode(&gray_frame(640, 480, i)).unwrap());
		}
		frames.extend(encoder.finish().unwrap());

		assert!(!frames.is_empty(), "encoder produced no packets");
		// The MFT buffers, so packets come back stamped with the frame they were
		// encoded from rather than whichever frame was going in at the time.
		let micros: Vec<u128> = frames.iter().map(|f| f.timestamp.as_micros()).collect();
		assert!(
			micros.windows(2).all(|w| w[0] < w[1]),
			"encoded timestamps not strictly increasing: {micros:?}"
		);
		assert!(
			micros.iter().all(|&t| t % 33_333 == 0 && t < 30 * 33_333),
			"encoded timestamp outside the fed set: {micros:?}"
		);

		let packets = payloads(&frames);
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

		let mut frames = Vec::new();
		let mut textures = 0;
		for i in 0..30 {
			let surface = camera.read().await.expect("frame, not end of stream");
			if matches!(surface, Surface::Texture(_)) {
				textures += 1;
			}
			if i == 0 {
				encoder.keyframe();
			}
			frames.extend(encoder.encode(&Frame::new(surface, at(i))).unwrap());
		}
		frames.extend(encoder.finish().unwrap());

		// On a GPU this exercises the zero-copy texture path; the assert guards
		// against silently testing only the CPU fallback.
		assert!(textures > 0, "capture never produced a GPU texture");
		assert!(!frames.is_empty(), "encoder produced no packets");
		let packets = payloads(&frames);
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

		let opened = encoder.bitrate();
		assert_eq!(opened, config.resolved_bitrate());

		// Encode first: this is the live-retune path, once the encoder exists.
		encoder.encode(&gray_frame(320, 240, 0)).unwrap();

		let halved = opened / 2;
		encoder.set_bitrate(halved).unwrap();
		assert_eq!(encoder.bitrate(), halved);

		// The retuned encoder must still emit a decodable keyframe, not wedge.
		let frames = encoder.encode(&gray_frame(320, 240, 1)).unwrap();
		assert!(!frames.is_empty(), "encoder produced nothing after a retune");
		let packets = payloads(&frames);
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
		let frames = encoder.encode(&gray_frame(320, 240, 0)).unwrap();
		assert!(!frames.is_empty());

		// And the encoder is live now, so a further retune takes the direct path.
		encoder.set_bitrate(halved / 2).unwrap();
		assert!(encoder.encode(&gray_frame(320, 240, 1)).is_ok());
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

	/// A backend that holds each frame back by one, like the Media Foundation MFT:
	/// `encode` returns the *previous* frame's access unit and `finish` drains the
	/// last. The payload is the frame's timestamp in microseconds, so a test can
	/// tell which frame a packet came from independently of what it's stamped with.
	struct Delayed {
		pending: Option<Encoded>,
	}

	impl Backend for Delayed {
		fn encode(&mut self, frame: &Frame, _keyframe: bool) -> Result<Vec<Encoded>, Error> {
			let payload = bytes::Bytes::from(frame.timestamp.as_micros().to_string());
			let previous = self.pending.replace(Encoded::new(payload, frame.timestamp));
			Ok(previous.into_iter().collect())
		}

		fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
			Ok(self.pending.take().into_iter().collect())
		}

		fn finish(&mut self) -> Result<Vec<Encoded>, Error> {
			self.flush()
		}

		fn set_bitrate(&mut self, _bitrate: u64) -> Result<(), Error> {
			Ok(())
		}

		fn name(&self) -> &str {
			"delayed"
		}
	}

	/// An encoder over a hand-built backend, so a test can pick the buffering
	/// behavior rather than take whatever this machine's hardware does.
	fn encoder_with(backend: Box<dyn Backend>, config: &Config) -> Encoder {
		Encoder {
			backend,
			codec: config.codec,
			size: config.size(),
			bitrate: config.resolved_bitrate(),
			color: config.resolved_color(),
			pending_keyframe: false,
		}
	}

	/// Records the keyframe flag each frame reached the codec with, so a test can
	/// check what the encoder actually asked for.
	struct Recorder(std::sync::Arc<std::sync::Mutex<Vec<bool>>>);

	impl Backend for Recorder {
		fn encode(&mut self, _frame: &Frame, keyframe: bool) -> Result<Vec<Encoded>, Error> {
			self.0.lock().unwrap().push(keyframe);
			Ok(Vec::new())
		}

		fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
			Ok(Vec::new())
		}

		fn finish(&mut self) -> Result<Vec<Encoded>, Error> {
			Ok(Vec::new())
		}

		fn set_bitrate(&mut self, _bitrate: u64) -> Result<(), Error> {
			Ok(())
		}

		fn name(&self) -> &str {
			"recorder"
		}
	}

	/// Keyframes are automatic, so an untouched encoder forces none. A request is
	/// held until a frame arrives (callers decide a group boundary before they have
	/// the frame that opens it), collapses if made twice, and clears afterwards
	/// rather than keying every frame from then on.
	#[test]
	fn a_keyframe_request_waits_for_the_next_frame_then_clears() {
		let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
		let config = Config::new(320, 240, 30);
		let mut encoder = encoder_with(Box::new(Recorder(log.clone())), &config);

		// Nothing asked for: `Config::gop` keys the stream on its own.
		encoder.encode(&gray_frame(320, 240, 0)).unwrap();

		// Asked twice before a frame exists: one keyframe, on the next frame.
		encoder.keyframe();
		encoder.keyframe();
		encoder.encode(&gray_frame(320, 240, 1)).unwrap();

		// And it does not carry into the frame after.
		encoder.encode(&gray_frame(320, 240, 2)).unwrap();

		assert_eq!(*log.lock().unwrap(), vec![false, true, false]);
	}

	/// `keyframe()` has to reach the codec on a *warm* encoder, which is the case
	/// that matters: a fresh one emits an IDR on its first frame regardless, so only
	/// a mid-stream request proves the plumbing works. Runs on openh264, so this
	/// holds on every platform rather than only where hardware exists.
	#[test]
	fn a_mid_stream_keyframe_request_emits_an_idr() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		// A GOP far longer than the run, so any IDR here was asked for rather than
		// inserted on schedule.
		let mut encoder = Encoder::new(&Config { gop: 1000, ..config }).unwrap();

		let mut per_frame = Vec::new();
		for i in 0..6 {
			// Frame 0 opens the stream; ask again at frame 3, mid-stream.
			if i == 3 {
				encoder.keyframe();
			}
			let encoded = encoder.encode(&gray_frame(320, 240, i)).unwrap();
			let joined: Vec<u8> = encoded.iter().flat_map(|f| f.payload.iter()).copied().collect();
			per_frame.push(nal_types(&joined));
		}

		// The requested frame is a self-contained IDR: SPS (7) and PPS (8) inline
		// ahead of an IDR slice (5), which is what avc3 promises a joining subscriber.
		let asked = &per_frame[3];
		assert!(asked.contains(&5), "the requested frame is not an IDR: {asked:?}");
		assert!(asked.contains(&7), "no SPS with the requested IDR: {asked:?}");
		assert!(asked.contains(&8), "no PPS with the requested IDR: {asked:?}");

		// And the frames around it stay delta frames, so the request keyed exactly
		// one: keying every frame would pass the assertions above while destroying
		// the bitrate.
		for i in [1, 2, 4, 5] {
			assert!(
				!per_frame[i].contains(&5),
				"frame {i} was keyed without being asked: {:?}",
				per_frame[i]
			);
		}
	}

	/// A backend whose every encode fails, to pin what survives one.
	struct Failing;

	impl Backend for Failing {
		fn encode(&mut self, _frame: &Frame, _keyframe: bool) -> Result<Vec<Encoded>, Error> {
			Err(Error::Codec(anyhow::anyhow!("no")))
		}

		fn flush(&mut self) -> Result<Vec<Encoded>, Error> {
			Ok(Vec::new())
		}

		fn finish(&mut self) -> Result<Vec<Encoded>, Error> {
			Ok(Vec::new())
		}

		fn set_bitrate(&mut self, _bitrate: u64) -> Result<(), Error> {
			Ok(())
		}

		fn name(&self) -> &str {
			"failing"
		}
	}

	/// A request outlives an encode that produced no picture, whether it was
	/// rejected up front (wrong size) or failed in the backend. Dropping it would
	/// leave the next frame unkeyed, so a subscriber waits out a whole GOP for a
	/// starting point the caller already asked for.
	#[test]
	fn a_failed_encode_keeps_the_keyframe_request() {
		let config = Config::new(320, 240, 30);

		let mut encoder = encoder_with(Box::new(Failing), &config);
		encoder.keyframe();
		assert!(encoder.encode(&gray_frame(320, 240, 0)).is_err());
		assert!(encoder.pending_keyframe, "the backend error swallowed the request");

		// Rejected before the backend ever sees it, for the same reason.
		let mut encoder = encoder_with(Box::new(Delayed { pending: None }), &config);
		encoder.keyframe();
		assert!(encoder.encode(&gray_frame(640, 480, 0)).is_err());
		assert!(encoder.pending_keyframe, "the size check swallowed the request");

		// ...and the next frame that does go through claims it.
		encoder.encode(&gray_frame(320, 240, 1)).unwrap();
		assert!(!encoder.pending_keyframe);
	}

	/// Regression: a backend that buffers hands back an earlier frame's access
	/// unit, so the timestamp has to ride through the encoder with the picture.
	/// Stamping output with whatever frame is going in at the time (or the tail
	/// with one arbitrary time) shifts every packet by the encoder delay and
	/// collapses the drained tail onto a single instant.
	#[test]
	fn a_buffering_backend_keeps_each_frames_timestamp() {
		let config = Config::new(320, 240, 30);
		let mut encoder = encoder_with(Box::new(Delayed { pending: None }), &config);

		// The packet handed back while frame `i` goes in belongs to frame `i - 1`, so
		// it has to be stamped one frame back. Stamping at the call site (the only
		// option when encode just returned bytes) would shift the whole stream.
		for i in 0..5 {
			let encoded = encoder.encode(&gray_frame(320, 240, i)).unwrap();
			if i == 0 {
				assert!(encoded.is_empty(), "the first frame is still buffered");
				continue;
			}
			assert_eq!(encoded.len(), 1);
			assert_eq!(encoded[0].timestamp, at(i - 1));
			// And the packet really is the earlier frame's, not a mis-stamped copy of
			// the one going in.
			assert_eq!(&encoded[0].payload[..], at(i - 1).as_micros().to_string().as_bytes());
		}

		// The tail: frame 4 never came back from an encode call, so `finish` drains
		// it, still carrying its own time rather than one the caller picks.
		let tail = encoder.finish().unwrap();
		assert_eq!(tail.len(), 1);
		assert_eq!(tail[0].timestamp, at(4));
		assert!(
			encoder_with(Box::new(Delayed { pending: None }), &config)
				.finish()
				.unwrap()
				.is_empty()
		);
	}

	/// A group has to contain its own frames. A codec that pipelines is still
	/// holding the last of them when the group ends, so the boundary flushes it;
	/// without that they surface in the *next* group, ahead of its keyframe, where
	/// a subscriber joining there cannot decode them.
	///
	/// The encoder stays usable afterwards, which is what separates this from
	/// `finish`: a live track flushes at every group and keeps going.
	#[test]
	fn a_flush_empties_a_pipelined_backend_and_leaves_it_running() {
		let config = Config::new(320, 240, 30);
		let mut encoder = encoder_with(Box::new(Delayed { pending: None }), &config);

		let mut group = Vec::new();
		for i in 0..3 {
			group.extend(encoder.encode(&gray_frame(320, 240, i)).unwrap());
		}
		group.extend(encoder.flush().unwrap());

		// All three frames land in the group they belong to, in order.
		let times: Vec<_> = group.iter().map(|packet| packet.timestamp).collect();
		assert_eq!(times, vec![at(0), at(1), at(2)], "the group lost or reordered frames");

		// The next group starts clean: nothing carried over, and the encoder still
		// takes frames.
		let mut next = encoder.encode(&gray_frame(320, 240, 3)).unwrap();
		assert!(next.is_empty(), "frame 3 is buffered, so nothing comes back yet");
		next.extend(encoder.finish().unwrap());
		let times: Vec<_> = next.iter().map(|packet| packet.timestamp).collect();
		assert_eq!(times, vec![at(3)], "the flush left something behind");
	}

	/// The hardware encoder states the color space too, and states the one the
	/// pixels were actually converted into. VideoToolbox takes the three
	/// properties as a request, so read the SPS back rather than trusting that it
	/// honored them.
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_sps_declares_the_color_space() {
		use super::backend::test_util::{BT601_DESCRIBED, BT709_DESCRIBED, declared_color};

		for (size, described) in [
			(Size::new(640, 480), BT601_DESCRIBED),
			(Size::new(1920, 1080), BT709_DESCRIBED),
		] {
			let config = Config {
				kind: Kind::Named("videotoolbox".into()),
				..Config::new(size.width, size.height, 30)
			};
			let mut encoder = Encoder::new(&config).expect("videotoolbox is available on macOS");

			let rgba = [255u8, 0, 0, 255].repeat(size.pixels() as usize);
			let surface = crate::frame::Surface::rgba(&rgba, size).unwrap();
			encoder.keyframe();
			let frames = encoder
				.encode(&Frame::new(surface, moq_net::Timestamp::from_micros(0).unwrap()))
				.unwrap();

			let keyframe = frames.first().expect("a keyframe");
			assert_eq!(declared_color(&keyframe.payload), Some(described), "{size} SPS");
		}
	}

	/// Resizing across the standard-definition boundary keeps the pixels' color
	/// space but moves the encoder into another one, which would emit BT.709
	/// pixels under a BT.601 label. `Config::color` pins the real space, and the
	/// bitstream then says so.
	#[test]
	fn config_color_pins_the_space_a_resize_carried() {
		use crate::Color;

		let big = Size::new(1280, 720);
		let small = Size::new(640, 480);

		// Converted at 720p, so the samples are BT.709.
		let rgba = vec![0x80u8; big.pixels() as usize * 4];
		let frame = Frame::new(
			crate::frame::Surface::rgba(&rgba, big).unwrap(),
			moq_net::Timestamp::from_micros(0).unwrap(),
		);
		let scaled = frame.resize(small).unwrap();
		assert_eq!(
			scaled.surface.color(),
			Some(Color::Bt709Limited),
			"resize keeps the space"
		);

		// Left to infer, a 480p encoder writes BT.601 over those BT.709 samples. It
		// still encodes (a live gateway keeps serving) but the label is wrong, which
		// is what `Config::color` exists to fix.
		let config = Config {
			kind: Kind::Software,
			..Config::new(small.width, small.height, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();
		encoder.keyframe();
		let frames = encoder.encode(&scaled).expect("a mismatch warns rather than fails");
		use super::backend::test_util::{BT601_DESCRIBED, BT709_DESCRIBED, declared_color};
		assert_eq!(
			declared_color(&frames.first().expect("a keyframe").payload),
			Some(BT601_DESCRIBED),
			"the inferred label is the wrong one, which is the case Config::color covers"
		);

		// Declaring the real space fixes the label.
		let config = Config {
			kind: Kind::Software,
			color: Some(Color::Bt709Limited),
			..Config::new(small.width, small.height, 30)
		};
		let mut encoder = Encoder::new(&config).unwrap();
		encoder.keyframe();
		let frames = encoder.encode(&scaled).expect("a declared space encodes");

		let keyframe = frames.first().expect("a keyframe");
		assert_eq!(declared_color(&keyframe.payload), Some(BT709_DESCRIBED));
	}
}
