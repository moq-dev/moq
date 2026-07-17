//! Video decoder front end.
//!
//! Prepares each container frame for a [`Backend`](super::backend::Backend):
//! converts out-of-band payloads (avc1 / hvc1: length-prefixed NALs with the
//! parameter sets in the description) to Annex-B and injects those parameter sets
//! ahead of keyframes, leaving in-band H.264 / H.265 payloads (avc3 / hev1,
//! already Annex-B inline) and AV1 OBU temporal units untouched. Gates output
//! until the first keyframe so the backend never sees a delta frame it can't
//! decode.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{AV1, VideoCodec, VideoConfig};
use moq_mux::codec::{annexb, h264, h265};
use moq_net::Timestamp;

use super::Frame;
use super::backend::{self, Backend, Codec};
use crate::{Error, Size};

/// Which decoder implementation to use. `#[non_exhaustive]` so new selection
/// strategies can be added without breaking external `match`es.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Kind {
	/// Prefer a platform hardware decoder, fall back to software.
	#[default]
	Auto,
	/// Hardware only; error if none is available.
	Hardware,
	/// Software (openh264) only.
	Software,
	/// A specific backend by name, e.g. `"videotoolbox"`, `"nvdec"`,
	/// `"openh264"`.
	Named(String),
}

/// Decoder configuration.
///
/// `#[non_exhaustive]`: build via [`Config::new`] (or `default()`) and set the
/// optional fields, so future knobs don't break callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Which backend to use.
	pub kind: Kind,
	/// Upper bound on buffering before a stalled group is skipped. `None` uses
	/// the moq-mux default (skip aggressively); set it to your playout buffer for
	/// a softer skip. Forwarded to the container consumer's `with_latency`.
	pub latency_max: Option<Duration>,
	/// Ask the decoder to emit frames at this size (both dimensions even) instead
	/// of the stream's native one. Best effort: a hardware decoder with a
	/// built-in scaler (NVDEC) honors it for free, other backends ignore it.
	/// Check each [`Frame`](super::Frame)'s dimensions and scale the remainder
	/// yourself.
	pub resize: Option<Size>,
}

impl Config {
	/// A default config: automatic backend selection, default latency.
	pub fn new() -> Self {
		Self::default()
	}
}

/// How to turn a container payload into a backend access unit.
enum Conversion {
	/// The payload is already in the backend's input framing: Annex-B for avc3 /
	/// hev1, OBU temporal units for AV1.
	Passthrough,
	/// avc1 / hvc1: length-prefixed NALs with the parameter sets out-of-band (in
	/// the avcC / hvcC description). Replace the length prefixes with start codes
	/// and prepend `keyframe_prefix` (the parameter sets) ahead of every keyframe.
	LengthPrefixed { length_size: usize, keyframe_prefix: Bytes },
}

/// Decodes container payloads (the codec bitstream) into raw [`Frame`]s.
///
/// The bring-your-own-payload layer under [`Consumer`](super::Consumer): use it
/// when the frames don't come from a plain track subscription, e.g. a transcoder
/// serving individually fetched groups. Feed it the payload of each container
/// frame in decode order; it handles avc1/hvc1 -> Annex-B conversion, passes
/// AV1 OBU temporal units through, and gates output until the first keyframe.
pub struct Decoder {
	backend: Box<dyn Backend>,
	conversion: Conversion,
	got_keyframe: bool,
}

impl Decoder {
	/// Build a decoder for the catalog's video config. Errors if the codec is
	/// not supported by the native backends.
	pub fn new(catalog: &VideoConfig, config: &Config) -> Result<Self, Error> {
		let (codec, conversion) = match &catalog.codec {
			VideoCodec::H264(h264) => {
				let conversion = if h264.inline {
					Conversion::Passthrough
				} else {
					let avcc = catalog.description.as_ref().ok_or_else(|| {
						Error::Codec(anyhow::anyhow!("avc1 H.264 track is missing its avcC description"))
					})?;
					let params = h264::Avcc::parse(avcc).map_err(moq_mux::Error::from)?;
					let keyframe_prefix = annexb::build_prefix(params.sps.iter().chain(params.pps.iter()));
					Conversion::LengthPrefixed {
						length_size: params.length_size,
						keyframe_prefix,
					}
				};
				(Codec::H264, conversion)
			}
			VideoCodec::H265(h265) => {
				let conversion = if h265.in_band {
					Conversion::Passthrough
				} else {
					let hvcc = catalog.description.as_ref().ok_or_else(|| {
						Error::Codec(anyhow::anyhow!("hvc1 H.265 track is missing its hvcC description"))
					})?;
					let params = h265::Hvcc::parse(hvcc).map_err(moq_mux::Error::from)?;
					let keyframe_prefix =
						annexb::build_prefix(params.vps.iter().chain(params.sps.iter()).chain(params.pps.iter()));
					Conversion::LengthPrefixed {
						length_size: params.length_size,
						keyframe_prefix,
					}
				};
				(Codec::H265, conversion)
			}
			VideoCodec::AV1(av1) if is_supported_av1(av1) => (Codec::Av1, Conversion::Passthrough),
			other => return Err(Error::UnsupportedCodec(other.to_string())),
		};

		let backend = backend::open(codec, config)?;
		tracing::debug!(decoder = backend.name(), "opened video decoder");
		Ok(Self {
			backend,
			conversion,
			got_keyframe: false,
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"`.
	pub fn name(&self) -> &str {
		self.backend.name()
	}

	/// Decode one container frame, returning zero or more raw frames. `timestamp` is
	/// this frame's presentation time; it rides through the decoder and comes back on
	/// each output frame, so a reordering decoder (B-frames) stamps every picture
	/// with its own presentation time rather than this access unit's. With no
	/// reordering the two coincide.
	pub fn decode(&mut self, payload: &Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Frame>, Error> {
		// Wait for the first keyframe: a decoder started mid-GOP can't decode
		// delta frames, and the parameter sets ride along with the keyframe.
		if !self.got_keyframe {
			if !keyframe {
				return Ok(Vec::new());
			}
			self.got_keyframe = true;
		}

		let access_unit = match &self.conversion {
			// Cheap refcount bump; the backend splits codec units off this buffer.
			Conversion::Passthrough => payload.clone(),
			Conversion::LengthPrefixed {
				length_size,
				keyframe_prefix,
			} => {
				let prefix = keyframe.then(|| keyframe_prefix.as_ref());
				annexb::from_length_prefixed(payload, *length_size, prefix).map_err(moq_mux::Error::from)?
			}
		};

		Ok(self
			.backend
			.decode(access_unit, timestamp, keyframe)?
			.into_iter()
			.map(|decoded| Frame {
				timestamp: decoded.timestamp,
				size: Size::new(decoded.frame.width(), decoded.frame.height()),
				inner: decoded.frame,
			})
			.collect())
	}
}

fn is_supported_av1(av1: &AV1) -> bool {
	av1.bitdepth == 8 && !av1.mono_chrome && av1.chroma_subsampling_x && av1.chroma_subsampling_y
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::backend::{self, Codec};
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind};
	use crate::frame::I420;

	/// A mid-gray RGBA frame: encodable without a camera.
	fn gray_rgba(width: u32, height: u32) -> Vec<u8> {
		vec![0x80u8; width as usize * height as usize * 4]
	}

	/// Assert a decoded picture is the expected size and looks like the gray frame
	/// we encoded. Mid-gray RGBA (0x80) is a flat picture: BT.601 limited-range
	/// luma near 125 and neutral chroma near 128. Averaging each plane catches
	/// plane swaps, stride bugs, and a misread Y/UV split that a size check misses.
	fn assert_gray(i420: &I420, width: u32, height: u32) {
		assert_eq!(i420.width, width);
		assert_eq!(i420.height, height);
		let luma = (width * height) as usize;
		// Tightly-packed I420: luma + two quarter-size chroma planes.
		assert_eq!(i420.data.len(), luma * 3 / 2);

		let avg = |plane: &[u8]| plane.iter().map(|&b| b as u32).sum::<u32>() / plane.len() as u32;
		let y = avg(&i420.data[..luma]);
		let u = avg(&i420.data[luma..luma + luma / 4]);
		let v = avg(&i420.data[luma + luma / 4..]);
		assert!((110..=140).contains(&y), "luma {y} off for a gray frame");
		assert!((118..=138).contains(&u), "u {u} off for a gray frame");
		assert!((118..=138).contains(&v), "v {v} off for a gray frame");
	}

	/// Encode 10 gray frames with `encoder`, decode them through `decoder`, and
	/// assert each decoded picture round-trips. Keyframe gating is exercised (the
	/// first packet is a keyframe with inline parameter sets).
	fn round_trip(mut encoder: Encoder, mut decoder: Box<dyn backend::Backend>, expect_name: &str) {
		assert_eq!(decoder.name(), expect_name);

		let frame = gray_rgba(320, 240);
		let mut decoded = Vec::new();
		for i in 0..10u64 {
			let keyframe = i == 0;
			// Distinct, spread-apart timestamps so a round-tripped value is unambiguous.
			let timestamp = Timestamp::from_micros(i * 33_333).unwrap();
			for packet in encoder
				.encode_rgba(&frame, crate::Size::new(320, 240), keyframe)
				.unwrap()
			{
				decoded.extend(decoder.decode(packet, timestamp, keyframe).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "decoder produced no frames");
		for out in &decoded {
			assert_gray(&out.frame.to_i420().unwrap(), 320, 240);
		}

		// The timestamp rides through the codec and comes back on each picture. These
		// backends don't reorder, so it returns in feed order: strictly increasing and
		// drawn from the values we fed.
		let micros: Vec<u128> = decoded.iter().map(|d| d.timestamp.as_micros()).collect();
		assert!(
			micros.windows(2).all(|w| w[0] < w[1]),
			"decoded timestamps not strictly increasing: {micros:?}"
		);
		assert!(
			micros.iter().all(|&t| t % 33_333 == 0 && t < 333_330),
			"decoded timestamp outside the fed set: {micros:?}"
		);
	}

	/// A decoder config selecting one backend by kind.
	fn decode_config(kind: super::Kind) -> super::Config {
		super::Config {
			kind,
			..super::Config::new()
		}
	}

	/// An openh264 (software H.264) encoder for the gray test stream.
	fn h264_software_encoder() -> Encoder {
		Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(320, 240, 30)
		})
		.expect("openh264 encoder")
	}

	#[test]
	fn openh264_round_trip() {
		let decoder = backend::open(Codec::H264, &decode_config(super::Kind::Software)).expect("openh264 decoder");
		round_trip(h264_software_encoder(), decoder, "openh264");
	}

	#[test]
	fn av1_is_supported_by_hardware_only() {
		let catalog = hang::catalog::VideoConfig::new(hang::catalog::AV1::default());
		let config = decode_config(super::Kind::Software);
		let Err(err) = super::Decoder::new(&catalog, &config) else {
			panic!("software AV1 decode unexpectedly opened");
		};
		assert!(matches!(err, crate::Error::NoDecoder(_)));
	}

	#[test]
	fn av1_rejects_unsupported_catalog_shape() {
		let av1 = hang::catalog::AV1 {
			bitdepth: 10,
			..hang::catalog::AV1::default()
		};
		let catalog = hang::catalog::VideoConfig::new(av1);
		let config = decode_config(super::Kind::Auto);
		let Err(err) = super::Decoder::new(&catalog, &config) else {
			panic!("10-bit AV1 decode unexpectedly opened");
		};
		assert!(matches!(err, crate::Error::UnsupportedCodec(_)));
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_round_trip() {
		let decoder = backend::open(Codec::H264, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox decoder");
		round_trip(h264_software_encoder(), decoder, "videotoolbox");
	}

	/// H.265 has no software path, so the HEVC round-trip rides VideoToolbox on
	/// both ends: hardware HEVC encode emitting hev1 (inline VPS/SPS/PPS) and
	/// hardware HEVC decode. Skips cleanly on a Mac without HEVC hardware (older
	/// Intel models predating the HEVC encoder).
	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_hevc_round_trip() {
		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("videotoolbox".into()),
			codec: crate::encode::Codec::H265,
			..EncodeConfig::new(320, 240, 30)
		});
		let Ok(encoder) = encoder else {
			eprintln!("skipping: no VideoToolbox H.265 hardware encoder available");
			return;
		};
		let decoder = backend::open(Codec::H265, &decode_config(super::Kind::Named("videotoolbox".into())))
			.expect("videotoolbox H.265 decoder");
		round_trip(encoder, decoder, "videotoolbox");
	}

	#[cfg(target_os = "windows")]
	#[test]
	fn mediafoundation_round_trip() {
		// Requires a hardware decoder MFT (GPU). Skip on machines without one
		// rather than fail: CI runners are often headless.
		let Ok(decoder) = backend::open(
			Codec::H264,
			&decode_config(super::Kind::Named("mediafoundation".into())),
		) else {
			eprintln!("skipping: no Media Foundation H.264 hardware decoder available");
			return;
		};
		round_trip(h264_software_encoder(), decoder, "mediafoundation");
	}

	/// H.265 has no software encoder or decoder, so the HEVC round-trip rides the
	/// Media Foundation hardware path on both ends: NVENC/QSV/AMF encode through an
	/// HEVC encoder MFT, DXVA decode through an HEVC decoder MFT. Skips cleanly when
	/// either is absent (no GPU, or no HEVC Video Extensions installed).
	#[cfg(target_os = "windows")]
	#[test]
	fn mediafoundation_hevc_round_trip() {
		let encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Named("mediafoundation".into()),
			codec: crate::encode::Codec::H265,
			..EncodeConfig::new(320, 240, 30)
		});
		let Ok(encoder) = encoder else {
			eprintln!("skipping: no Media Foundation H.265 hardware encoder available");
			return;
		};
		let Ok(decoder) = backend::open(
			Codec::H265,
			&decode_config(super::Kind::Named("mediafoundation".into())),
		) else {
			eprintln!("skipping: no Media Foundation H.265 hardware decoder available");
			return;
		};
		round_trip(encoder, decoder, "mediafoundation");
	}
}
