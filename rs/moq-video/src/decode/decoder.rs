//! H.264 / H.265 decoder front end.
//!
//! Prepares each container frame for a [`Backend`](super::backend::Backend):
//! converts out-of-band payloads (avc1 / hvc1: length-prefixed NALs with the
//! parameter sets in the description) to Annex-B and injects those parameter sets
//! ahead of keyframes, leaving in-band payloads (avc3 / hev1, already Annex-B
//! inline) untouched. Gates output until the first keyframe so the backend never
//! sees a delta frame it can't decode.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{VideoCodec, VideoConfig};
use moq_mux::codec::{annexb, h264, h265};

use super::backend::{self, Backend, Codec};
use crate::Error;
use crate::frame::I420;

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
	/// A specific backend by name, e.g. `"videotoolbox"`, `"openh264"`.
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
}

impl Config {
	/// A default config: automatic backend selection, default latency.
	pub fn new() -> Self {
		Self::default()
	}
}

/// How to turn a container payload into an Annex-B access unit for the backend.
enum Conversion {
	/// avc3 / hev1: the payload is already Annex-B with parameter sets inline.
	/// Pass through.
	Annexb,
	/// avc1 / hvc1: length-prefixed NALs with the parameter sets out-of-band (in
	/// the avcC / hvcC description). Replace the length prefixes with start codes
	/// and prepend `keyframe_prefix` (the parameter sets) ahead of every keyframe.
	LengthPrefixed { length_size: usize, keyframe_prefix: Bytes },
}

/// Drives a [`Backend`] from container frames.
pub(crate) struct Decoder {
	backend: Box<dyn Backend>,
	conversion: Conversion,
	got_keyframe: bool,
}

impl Decoder {
	/// Build a decoder for the catalog's video config. Errors if the codec is
	/// neither H.264 nor H.265 (the codecs the native backends support).
	pub(crate) fn new(catalog: &VideoConfig, kind: &Kind) -> Result<Self, Error> {
		let (codec, conversion) = match &catalog.codec {
			VideoCodec::H264(h264) => {
				let conversion = if h264.inline {
					Conversion::Annexb
				} else {
					let avcc = catalog.description.as_ref().ok_or_else(|| {
						Error::Codec(anyhow::anyhow!("avc1 H.264 track is missing its avcC description"))
					})?;
					let params = h264::parse_avcc_param_sets(avcc).map_err(moq_mux::Error::from)?;
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
					Conversion::Annexb
				} else {
					let hvcc = catalog.description.as_ref().ok_or_else(|| {
						Error::Codec(anyhow::anyhow!("hvc1 H.265 track is missing its hvcC description"))
					})?;
					let params = h265::parse_hvcc_param_sets(hvcc).map_err(moq_mux::Error::from)?;
					let keyframe_prefix =
						annexb::build_prefix(params.vps.iter().chain(params.sps.iter()).chain(params.pps.iter()));
					Conversion::LengthPrefixed {
						length_size: params.length_size,
						keyframe_prefix,
					}
				};
				(Codec::H265, conversion)
			}
			other => return Err(Error::UnsupportedCodec(other.to_string())),
		};

		let backend = backend::open(codec, kind)?;
		tracing::debug!(decoder = backend.name(), "opened video decoder");
		Ok(Self {
			backend,
			conversion,
			got_keyframe: false,
		})
	}

	/// The decoder backend name in use, e.g. `"videotoolbox"`.
	pub(crate) fn name(&self) -> &str {
		self.backend.name()
	}

	/// Decode one container frame, returning zero or more raw I420 frames.
	pub(crate) fn decode(&mut self, payload: &Bytes, keyframe: bool) -> Result<Vec<I420>, Error> {
		// Wait for the first keyframe: a decoder started mid-GOP can't decode
		// delta frames, and the parameter sets ride along with the keyframe.
		if !self.got_keyframe {
			if !keyframe {
				return Ok(Vec::new());
			}
			self.got_keyframe = true;
		}

		let annexb = match &self.conversion {
			// Cheap refcount bump; the backend splits NAL slices off this buffer.
			Conversion::Annexb => payload.clone(),
			Conversion::LengthPrefixed {
				length_size,
				keyframe_prefix,
			} => {
				let prefix = keyframe.then(|| keyframe_prefix.as_ref());
				annexb::from_length_prefixed(payload, *length_size, prefix).map_err(moq_mux::Error::from)?
			}
		};

		self.backend.decode(annexb, keyframe)
	}
}

#[cfg(test)]
mod tests {
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
		for i in 0..10 {
			let keyframe = i == 0;
			for packet in encoder.encode_rgba(&frame, 320, 240, keyframe).unwrap() {
				decoded.extend(decoder.decode(packet, keyframe).unwrap());
			}
		}

		assert!(!decoded.is_empty(), "decoder produced no frames");
		for i420 in &decoded {
			assert_gray(i420, 320, 240);
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
		let decoder = backend::open(Codec::H264, &super::Kind::Software).expect("openh264 decoder");
		round_trip(h264_software_encoder(), decoder, "openh264");
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_round_trip() {
		let decoder =
			backend::open(Codec::H264, &super::Kind::Named("videotoolbox".into())).expect("videotoolbox decoder");
		round_trip(h264_software_encoder(), decoder, "videotoolbox");
	}

	#[cfg(target_os = "windows")]
	#[test]
	fn mediafoundation_round_trip() {
		// Requires a hardware decoder MFT (GPU). Skip on machines without one
		// rather than fail: CI runners are often headless.
		let Ok(decoder) = backend::open(Codec::H264, &super::Kind::Named("mediafoundation".into())) else {
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
		let Ok(decoder) = backend::open(Codec::H265, &super::Kind::Named("mediafoundation".into())) else {
			eprintln!("skipping: no Media Foundation H.265 hardware decoder available");
			return;
		};
		round_trip(encoder, decoder, "mediafoundation");
	}
}
