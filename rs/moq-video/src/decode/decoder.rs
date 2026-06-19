//! H.264 decoder front end.
//!
//! Prepares each container frame for a [`Backend`](super::backend::Backend):
//! converts avc1 (length-prefixed, out-of-band avcC) payloads to Annex-B and
//! injects the SPS/PPS ahead of keyframes, leaving avc3 (already Annex-B inline)
//! payloads untouched. Gates output until the first keyframe so the backend
//! never sees a delta frame it can't decode.

use std::time::Duration;

use bytes::Bytes;
use hang::catalog::{VideoCodec, VideoConfig};
use moq_mux::codec::{annexb, h264};

use super::backend::{self, Backend};
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
	/// avc3: the payload is already Annex-B with SPS/PPS inline. Pass through.
	Annexb,
	/// avc1: length-prefixed NALs with the avcC out-of-band. Replace the length
	/// prefixes with start codes and prepend `keyframe_prefix` (SPS/PPS from the
	/// avcC) ahead of every keyframe.
	Avc1 { length_size: usize, keyframe_prefix: Bytes },
}

/// Drives a [`Backend`] from container frames.
pub(crate) struct Decoder {
	backend: Box<dyn Backend>,
	conversion: Conversion,
	got_keyframe: bool,
}

impl Decoder {
	/// Build a decoder for the catalog's video config. Errors if the codec is not
	/// H.264 (the only codec the native backends support).
	pub(crate) fn new(catalog: &VideoConfig, kind: &Kind) -> Result<Self, Error> {
		let VideoCodec::H264(h264) = &catalog.codec else {
			return Err(Error::UnsupportedCodec(catalog.codec.to_string()));
		};

		let conversion = if h264.inline {
			Conversion::Annexb
		} else {
			let avcc = catalog
				.description
				.as_ref()
				.ok_or_else(|| Error::Codec(anyhow::anyhow!("avc1 H.264 track is missing its avcC description")))?;
			let params = h264::parse_avcc_param_sets(avcc).map_err(moq_mux::Error::from)?;
			let keyframe_prefix = annexb::build_prefix(params.sps.iter().chain(params.pps.iter()));
			Conversion::Avc1 {
				length_size: params.length_size,
				keyframe_prefix,
			}
		};

		let backend = backend::open(kind)?;
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
			Conversion::Avc1 {
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
	use super::backend;
	use crate::encode::{Config as EncodeConfig, Encoder, Kind as EncodeKind};

	/// A mid-gray RGBA frame: encodable without a camera.
	fn gray_rgba(width: u32, height: u32) -> Vec<u8> {
		vec![0x80u8; width as usize * height as usize * 4]
	}

	/// Encode synthetic frames to Annex-B, decode them back, and assert the
	/// backend hands us a correctly-sized I420 picture. Exercises the whole
	/// software path (openh264 encode -> openh264 decode), keyframe gating
	/// included (the first packet is an IDR with inline SPS/PPS).
	fn round_trip(decode_kind: &super::Kind, expect_name: &str) {
		let mut encoder = Encoder::new(&EncodeConfig {
			kind: EncodeKind::Software,
			..EncodeConfig::new(320, 240, 30)
		})
		.expect("openh264 encoder");

		let mut decoder = backend::open(decode_kind).expect("decoder available");
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
			assert_eq!(i420.width, 320);
			assert_eq!(i420.height, 240);
			// Tightly-packed I420: luma + two quarter-size chroma planes.
			assert_eq!(i420.data.len(), 320 * 240 * 3 / 2);
		}
	}

	#[test]
	fn openh264_round_trip() {
		round_trip(&super::Kind::Software, "openh264");
	}

	#[cfg(target_os = "macos")]
	#[test]
	fn videotoolbox_round_trip() {
		round_trip(&super::Kind::Named("videotoolbox".into()), "videotoolbox");
	}
}
