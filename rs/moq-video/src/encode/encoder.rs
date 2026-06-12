//! H.264 encoder over ffmpeg, hardware-preferred.
//!
//! Accepts decoded [`ffmpeg::frame::Video`] frames in any pixel format
//! (whatever the camera hands us), scales/converts them to YUV420P, and
//! emits Annex-B H.264 packets ready for `moq_mux::codec::h264::Import`.

use bytes::Bytes;
use ffmpeg_next as ffmpeg;

use crate::Error;

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
	/// Software (libx264 / built-in) only.
	Software,
	/// A specific ffmpeg encoder by name, e.g. `"h264_videotoolbox"`.
	Named(String),
}

/// Encoder configuration. `width` / `height` / `framerate` are the encoded
/// output; input frames are scaled/converted to match.
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
	fn resolved_bitrate(&self) -> u64 {
		self.bitrate.unwrap_or_else(|| {
			let pixels = self.width as u64 * self.height as u64;
			// 0.07 bits per pixel per second matches the JS publisher's
			// default and lands ~4.4 Mbps for 1080p30.
			((pixels * self.framerate as u64) as f64 * 0.07) as u64
		})
	}
}

/// Hardware H.264 encoder names to try first, in priority order. The deps
/// are declared under platform-specific cfgs in ffmpeg, but probing a name
/// that isn't compiled in just returns `None`, so listing all of them is
/// harmless on any platform.
const HARDWARE_ENCODERS: &[&str] = &[
	"h264_videotoolbox", // macOS / iOS
	"h264_nvenc",        // NVIDIA
	"h264_qsv",          // Intel QuickSync
	"h264_vaapi",        // Linux VA-API
	"h264_amf",          // AMD (Windows)
	"h264_v4l2m2m",      // Linux stateful (e.g. Raspberry Pi)
];

/// Software fallbacks, in priority order.
const SOFTWARE_ENCODERS: &[&str] = &["libx264", "h264"];

/// H.264 encoder. Build one with [`Encoder::new`], feed it raw RGBA frames
/// via [`encode_rgba`](Self::encode_rgba), and publish the resulting Annex-B
/// packets through [`Producer`](super::Producer).
pub struct Encoder {
	encoder: ffmpeg::encoder::video::Encoder,
	/// Lazily built once we see the first frame's pixel format/size.
	scaler: Option<Scaler>,
	width: u32,
	height: u32,
	frame_count: i64,
	/// The ffmpeg encoder name that opened successfully (for logging).
	name: String,
}

struct Scaler {
	ctx: ffmpeg::software::scaling::Context,
	src_format: ffmpeg::format::Pixel,
	src_width: u32,
	src_height: u32,
}

impl Encoder {
	pub fn new(config: &Config) -> Result<Self, Error> {
		// Validate at the construction boundary so both entry points (the
		// capture loop and a bring-your-own-frames caller) reject a zero
		// framerate, which would produce a degenerate `1/0` codec time base.
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

		// Idempotent; ensures codecs are registered even when no Camera opened.
		ffmpeg::init()?;
		let candidates = encoder_candidates(&config.kind);

		let mut tried = Vec::new();
		for name in &candidates {
			tried.push(name.clone());
			match open_encoder(name, config) {
				Ok(encoder) => {
					tracing::info!(encoder = %name, width = config.width, height = config.height, "opened H.264 encoder");
					return Ok(Self {
						encoder,
						scaler: None,
						width: config.width,
						height: config.height,
						frame_count: 0,
						name: name.clone(),
					});
				}
				Err(e) => {
					tracing::debug!(encoder = %name, error = %e, "encoder unavailable, trying next");
				}
			}
		}

		Err(Error::NoEncoder(tried.join(", ")))
	}

	/// The ffmpeg encoder name in use, e.g. `"h264_videotoolbox"`.
	pub fn name(&self) -> &str {
		&self.name
	}

	/// Encode one tightly-packed RGBA frame (`width * height * 4` bytes),
	/// returning zero or more Annex-B H.264 packets. Set `keyframe` to force an
	/// IDR (e.g. on resume so a re-subscribing viewer can start decoding at
	/// once). The frame is scaled/converted to the encoder's resolution.
	pub fn encode_rgba(&mut self, rgba: &[u8], width: u32, height: u32, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		let frame = rgba_frame(rgba, width, height)?;
		self.encode_frame(&frame, keyframe)
	}

	/// Encode a decoded frame (camera path). With B-frames disabled (the
	/// low-latency default) the encoder emits one packet per input frame.
	pub(crate) fn encode(&mut self, frame: &ffmpeg::frame::Video) -> Result<Vec<Bytes>, Error> {
		self.encode_frame(frame, false)
	}

	fn encode_frame(&mut self, frame: &ffmpeg::frame::Video, keyframe: bool) -> Result<Vec<Bytes>, Error> {
		let mut yuv = self.convert(frame)?;
		if keyframe {
			yuv.set_kind(ffmpeg::picture::Type::I);
		}
		self.encoder.send_frame(&yuv)?;
		self.drain()
	}

	/// Flush the encoder, returning any buffered packets.
	pub fn finish(&mut self) -> Result<Vec<Bytes>, Error> {
		self.encoder.send_eof()?;
		self.drain()
	}

	fn drain(&mut self) -> Result<Vec<Bytes>, Error> {
		let mut out = Vec::new();
		let mut packet = ffmpeg::Packet::empty();
		loop {
			match self.encoder.receive_packet(&mut packet) {
				Ok(()) => {
					if let Some(data) = packet.data() {
						out.push(Bytes::copy_from_slice(data));
					}
				}
				Err(ffmpeg::Error::Other { errno }) if errno == ffmpeg::util::error::EAGAIN => break,
				Err(ffmpeg::Error::Eof) => break,
				Err(e) => return Err(e.into()),
			}
		}
		Ok(out)
	}

	/// Scale/convert an arbitrary input frame to the encoder's YUV420P
	/// surface, rebuilding the scaler if the input geometry changed.
	fn convert(&mut self, frame: &ffmpeg::frame::Video) -> Result<ffmpeg::frame::Video, Error> {
		let (src_format, src_w, src_h) = (frame.format(), frame.width(), frame.height());

		let needs_rebuild = match &self.scaler {
			Some(s) => s.src_format != src_format || s.src_width != src_w || s.src_height != src_h,
			None => true,
		};
		if needs_rebuild {
			let ctx = ffmpeg::software::scaling::Context::get(
				src_format,
				src_w,
				src_h,
				ffmpeg::format::Pixel::YUV420P,
				self.width,
				self.height,
				ffmpeg::software::scaling::Flags::BILINEAR,
			)?;
			self.scaler = Some(Scaler {
				ctx,
				src_format,
				src_width: src_w,
				src_height: src_h,
			});
		}

		let scaler = self.scaler.as_mut().expect("scaler built above");
		let mut yuv = ffmpeg::frame::Video::empty();
		scaler.ctx.run(frame, &mut yuv)?;

		// The encoder times frames off a monotonic count, not the camera
		// clock; the moq presentation timestamp is attached downstream.
		yuv.set_pts(Some(self.frame_count));
		self.frame_count += 1;
		Ok(yuv)
	}
}

/// Wrap tightly-packed RGBA bytes in an ffmpeg frame, copying row-by-row to
/// honor ffmpeg's stride (which may exceed `width * 4`).
fn rgba_frame(rgba: &[u8], width: u32, height: u32) -> Result<ffmpeg::frame::Video, Error> {
	let row_bytes = width as usize * 4;
	let expected = row_bytes * height as usize;
	if rgba.len() < expected {
		return Err(Error::Codec(anyhow::anyhow!(
			"RGBA buffer too small: {} < {expected} for {width}x{height}",
			rgba.len()
		)));
	}

	let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGBA, width, height);
	let stride = frame.stride(0);
	for y in 0..height as usize {
		let src = y * row_bytes;
		let dst = y * stride;
		frame.data_mut(0)[dst..dst + row_bytes].copy_from_slice(&rgba[src..src + row_bytes]);
	}
	Ok(frame)
}

fn encoder_candidates(kind: &Kind) -> Vec<String> {
	match kind {
		Kind::Named(name) => vec![name.clone()],
		Kind::Hardware => HARDWARE_ENCODERS.iter().map(|s| s.to_string()).collect(),
		Kind::Software => SOFTWARE_ENCODERS.iter().map(|s| s.to_string()).collect(),
		Kind::Auto => HARDWARE_ENCODERS
			.iter()
			.chain(SOFTWARE_ENCODERS)
			.map(|s| s.to_string())
			.collect(),
	}
}

fn open_encoder(name: &str, config: &Config) -> Result<ffmpeg::encoder::video::Encoder, Error> {
	let codec = ffmpeg::encoder::find_by_name(name).ok_or_else(|| Error::NoEncoder(name.to_string()))?;

	let ctx = ffmpeg::codec::context::Context::new_with_codec(codec);
	let mut enc = ctx.encoder().video()?;
	enc.set_width(config.width);
	enc.set_height(config.height);
	enc.set_format(ffmpeg::format::Pixel::YUV420P);
	enc.set_time_base(ffmpeg::Rational::new(1, config.framerate as i32));
	enc.set_frame_rate(Some(ffmpeg::Rational::new(config.framerate as i32, 1)));
	enc.set_gop(config.gop);
	enc.set_max_b_frames(0); // Low latency: no reordering.
	enc.set_bit_rate(config.resolved_bitrate() as usize);

	let mut opts = ffmpeg::Dictionary::new();
	if name == "libx264" {
		opts.set("preset", "ultrafast");
		opts.set("tune", "zerolatency");
	} else if name == "h264_videotoolbox" {
		opts.set("realtime", "1");
		// Fall back to the software VideoToolbox path if no GPU encoder.
		opts.set("allow_sw", "1");
	}

	Ok(enc.open_with(opts)?)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A mid-gray YUV420P frame: encodable without a camera.
	fn gray_frame(width: u32, height: u32) -> ffmpeg::frame::Video {
		let mut frame = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUV420P, width, height);
		// Plane 0 is luma (gray = 128); planes 1/2 are chroma (neutral = 128).
		for plane in 0..frame.planes() {
			frame.data_mut(plane).fill(128);
		}
		frame
	}

	#[test]
	fn software_encoder_emits_annexb() {
		let config = Config {
			kind: Kind::Software,
			..Config::new(320, 240, 30)
		};
		let mut encoder = Encoder::new(&config).expect("libx264 should be available under nix ffmpeg");
		assert_eq!(encoder.name(), "libx264");

		let frame = gray_frame(320, 240);
		let mut packets = Vec::new();
		for _ in 0..30 {
			packets.extend(encoder.encode(&frame).unwrap());
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

		// Tightly-packed RGBA (width*height*4); the row-by-row copy must honor
		// ffmpeg's stride for this to decode.
		let rgba = vec![0x40u8; 320 * 240 * 4];
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
		// Far smaller than 320*240*4: must error, not panic on the row copy.
		assert!(matches!(
			encoder.encode_rgba(&[0u8; 16], 320, 240, false),
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

	#[test]
	fn default_bitrate_scales_with_resolution() {
		let small = Config::new(320, 240, 30).resolved_bitrate();
		let large = Config::new(1920, 1080, 30).resolved_bitrate();
		assert!(large > small);
		assert!(small > 0);
	}
}
