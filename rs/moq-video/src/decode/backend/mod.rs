//! Pluggable H.264 decoder backends.
//!
//! The mirror of [`encode::backend`](crate::encode). [`Backend`] is the seam
//! between the access-unit prep (avc1 -> Annex-B conversion + keyframe gating,
//! owned by [`Decoder`](super::Decoder)) and the codec itself. Every backend
//! takes one **Annex-B** H.264 access unit (SPS/PPS inline ahead of each
//! keyframe) and returns zero or more decoded [`I420`] frames.
//!
//! [`open`] picks the best backend for a [`Kind`](super::Kind), trying hardware
//! candidates (platform-gated) before the openh264 software fallback, exactly
//! like the encode side.

use bytes::Bytes;

use super::decoder::Kind;
use crate::Error;
use crate::frame::I420;

mod openh264;

#[cfg(target_os = "macos")]
mod videotoolbox;

/// An opened H.264 decoder. Feed it Annex-B access units in decode order; get
/// back zero or more raw I420 frames (zero while the decoder is still buffering,
/// e.g. before the first keyframe's parameter sets).
pub(crate) trait Backend: Send {
	/// Decode one Annex-B access unit. `keyframe` marks an IDR (parameter sets
	/// are inline ahead of it). Takes an owned [`Bytes`] so a backend can split
	/// out NAL slices without copying.
	fn decode(&mut self, access_unit: Bytes, keyframe: bool) -> Result<Vec<I420>, Error>;

	/// The decoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}

/// A backend constructor: name plus an opener that tries to start it.
struct Candidate {
	name: &'static str,
	open: fn() -> Result<Box<dyn Backend>, Error>,
}

/// Hardware backends, in priority order. Platform-gated so only the ones that
/// could plausibly work on this target are even listed.
const HARDWARE: &[Candidate] = &[
	#[cfg(target_os = "macos")]
	Candidate {
		name: videotoolbox::NAME,
		open: videotoolbox::VideoToolbox::open,
	},
];

const SOFTWARE: Candidate = Candidate {
	name: openh264::NAME,
	open: openh264::Openh264::open,
};

/// Open the best decoder for `kind`, trying candidates in priority order and
/// falling back until one succeeds.
pub(crate) fn open(kind: &Kind) -> Result<Box<dyn Backend>, Error> {
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
		tried.push(candidate.name);
		match (candidate.open)() {
			Ok(backend) => return Ok(backend),
			Err(e) => tracing::debug!(decoder = candidate.name, error = %e, "decoder unavailable, trying next"),
		}
	}

	Err(Error::NoDecoder(tried.join(", ")))
}
