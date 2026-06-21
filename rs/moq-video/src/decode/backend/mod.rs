//! Pluggable H.264 / H.265 decoder backends.
//!
//! The mirror of [`encode::backend`](crate::encode). [`Backend`] is the seam
//! between the access-unit prep (length-prefixed -> Annex-B conversion + keyframe
//! gating, owned by [`Decoder`](super::Decoder)) and the codec itself. Every
//! backend takes one **Annex-B** access unit (parameter sets inline ahead of each
//! keyframe: SPS/PPS for H.264, VPS/SPS/PPS for H.265) and returns zero or more
//! decoded [`I420`] frames.
//!
//! [`open`] picks the best backend for a ([`Codec`], [`Kind`](super::Kind)) pair,
//! trying hardware candidates (platform-gated: VideoToolbox on macOS, Media
//! Foundation / DXVA on Windows) before the openh264 software fallback, exactly
//! like the encode side. Only backends that support the requested codec are
//! considered: there is no software H.265 decoder, so an H.265 track has no
//! fallback below the hardware path.

use bytes::Bytes;

use super::decoder::Kind;
use crate::Error;
use crate::frame::I420;

mod openh264;

#[cfg(target_os = "macos")]
mod videotoolbox;

#[cfg(target_os = "windows")]
mod mediafoundation;

/// The video codec a decoder handles. Derived from the catalog, not chosen by the
/// caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Codec {
	H264,
	H265,
}

impl Codec {
	fn label(self) -> &'static str {
		match self {
			Codec::H264 => "H.264",
			Codec::H265 => "H.265",
		}
	}
}

/// An opened decoder. Feed it Annex-B access units in decode order; get back zero
/// or more raw I420 frames (zero while the decoder is still buffering, e.g. before
/// the first keyframe's parameter sets).
pub(crate) trait Backend: Send {
	/// Decode one Annex-B access unit. `keyframe` marks a keyframe (parameter sets
	/// are inline ahead of it). Takes an owned [`Bytes`] so a backend can split
	/// out NAL slices without copying.
	fn decode(&mut self, access_unit: Bytes, keyframe: bool) -> Result<Vec<I420>, Error>;

	/// The decoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}

/// A backend constructor: name, the codecs it can decode, and an opener.
struct Candidate {
	name: &'static str,
	supports: fn(Codec) -> bool,
	open: fn(Codec) -> Result<Box<dyn Backend>, Error>,
}

/// Hardware backends, in priority order. Platform-gated so only the ones that
/// could plausibly work on this target are even listed.
const HARDWARE: &[Candidate] = &[
	#[cfg(target_os = "macos")]
	Candidate {
		name: videotoolbox::NAME,
		supports: |c| matches!(c, Codec::H264),
		open: videotoolbox::VideoToolbox::open,
	},
	#[cfg(target_os = "windows")]
	Candidate {
		name: mediafoundation::NAME,
		supports: |_| true,
		open: mediafoundation::MediaFoundation::open,
	},
];

const SOFTWARE: Candidate = Candidate {
	name: openh264::NAME,
	supports: |c| matches!(c, Codec::H264),
	open: openh264::Openh264::open,
};

/// Open the best decoder for `codec` and `kind`, trying candidates in priority
/// order and falling back until one succeeds. Candidates that don't support the
/// codec are skipped before they're even tried.
pub(crate) fn open(codec: Codec, kind: &Kind) -> Result<Box<dyn Backend>, Error> {
	let candidates: Vec<&Candidate> = match kind {
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
		match (candidate.open)(codec) {
			Ok(backend) => return Ok(backend),
			Err(e) => tracing::debug!(decoder = candidate.name, error = %e, "decoder unavailable, trying next"),
		}
	}

	if tried.is_empty() {
		return Err(Error::NoDecoder(format!("none support {}", codec.label())));
	}
	Err(Error::NoDecoder(tried.join(", ")))
}
