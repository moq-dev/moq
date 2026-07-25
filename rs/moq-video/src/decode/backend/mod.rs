//! Pluggable video decoder backends.
//!
//! The mirror of [`encode::backend`](crate::encode). [`Backend`] is the seam
//! between the access-unit prep (keyframe gating plus any codec-specific payload
//! conversion, owned by [`Decoder`](super::Decoder)) and the codec itself. H.264
//! / H.265 backends take Annex-B access units with parameter sets inline ahead
//! of each keyframe; AV1 backends take OBU temporal units directly.
//!
//! [`open`] picks the best backend for a [`Codec`] and [`Config`], trying
//! hardware candidates (platform-gated: VideoToolbox on macOS, Media Foundation
//! / DXVA on Windows, NVDEC on Linux) before the openh264 software fallback,
//! exactly like the encode side. Only backends that support the requested codec
//! are considered: there is no software H.265 or AV1 decoder, so those tracks
//! have no fallback below the hardware path.

use bytes::Bytes;
use moq_net::Timestamp;

use super::decoder::{Config, Kind};
use crate::Error;
use crate::frame::Surface;

mod openh264;

#[cfg(target_os = "macos")]
mod videotoolbox;

#[cfg(target_os = "windows")]
mod mediafoundation;

#[cfg(all(target_os = "linux", feature = "nvdec"))]
mod nvdec;

/// The video codec a decoder handles. Derived from the catalog, not chosen by the
/// caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
	H264,
	H265,
	Av1,
}

impl Codec {
	fn label(self) -> &'static str {
		match self {
			Codec::H264 => "H.264",
			Codec::H265 => "H.265",
			Codec::Av1 => "AV1",
		}
	}
}

/// One decoded picture: the raw frame plus its presentation timestamp.
pub(crate) struct Decoded {
	/// Presentation timestamp. Backends that decode one-in one-out echo the input
	/// timestamp; NVDEC threads timestamps through its parser, so they survive
	/// decoder delay and frame reordering.
	pub timestamp: Timestamp,
	/// The decoded picture: CPU I420, or a GPU frame the encode side can consume
	/// without a CPU round trip.
	pub frame: Surface,
}

/// An opened decoder. Feed it prepared access units in decode order; get back
/// zero or more decoded frames (zero while the decoder is still buffering, e.g.
/// before the first keyframe's parameter sets).
pub(crate) trait Backend: Send {
	/// Decode one access unit stamped with its presentation `timestamp`.
	/// `keyframe` marks a random-access frame. Takes an owned [`Bytes`] so a
	/// backend can split codec units without copying.
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Decoded>, Error>;

	/// The decoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}

/// A backend opener: builds a decoder for a codec and config.
type Open = fn(Codec, &Config) -> Result<Box<dyn Backend>, Error>;

/// A backend constructor: name, the codecs it can decode, and an opener.
struct Candidate {
	name: &'static str,
	supports: fn(Codec) -> bool,
	open: Open,
}

/// Hardware backends, in priority order. Platform-gated so only the ones that
/// could plausibly work on this target are even listed.
const HARDWARE: &[Candidate] = &[
	#[cfg(target_os = "macos")]
	Candidate {
		name: videotoolbox::NAME,
		supports: |c| matches!(c, Codec::H264 | Codec::H265),
		open: videotoolbox::VideoToolbox::open,
	},
	#[cfg(target_os = "windows")]
	Candidate {
		name: mediafoundation::NAME,
		supports: |c| matches!(c, Codec::H264 | Codec::H265),
		open: mediafoundation::MediaFoundation::open,
	},
	#[cfg(all(target_os = "linux", feature = "nvdec"))]
	Candidate {
		name: nvdec::NAME,
		supports: |c| matches!(c, Codec::H264 | Codec::H265 | Codec::Av1),
		open: nvdec::Nvdec::open,
	},
];

const SOFTWARE: Candidate = Candidate {
	name: openh264::NAME,
	supports: |c| matches!(c, Codec::H264),
	open: openh264::Openh264::open,
};

/// Open the best decoder for `codec` and `config`, trying candidates in priority
/// order and falling back until one succeeds. Candidates that don't support the
/// codec are skipped before they're even tried.
pub(crate) fn open(codec: Codec, config: &Config) -> Result<Box<dyn Backend>, Error> {
	let candidates: Vec<&Candidate> = match &config.kind {
		Kind::Auto => HARDWARE.iter().chain(std::iter::once(&SOFTWARE)).collect(),
		Kind::Hardware => HARDWARE.iter().collect(),
		Kind::Software => vec![&SOFTWARE],
		Kind::Named(name) => {
			let all = HARDWARE.iter().chain(std::iter::once(&SOFTWARE));
			all.filter(|c| c.name == name).collect()
		}
	};

	let mut tried = Vec::new();
	for candidate in candidates {
		if !(candidate.supports)(codec) {
			continue;
		}
		tried.push(candidate.name);
		match (candidate.open)(codec, config) {
			Ok(backend) => return Ok(backend),
			Err(e) => tracing::debug!(decoder = candidate.name, error = %e, "decoder unavailable, trying next"),
		}
	}

	if tried.is_empty() {
		return Err(Error::NoDecoder(format!("none support {}", codec.label())));
	}
	Err(Error::NoDecoder(tried.join(", ")))
}
