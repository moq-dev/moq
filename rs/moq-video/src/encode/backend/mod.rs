//! Pluggable H.264 encoder backends.
//!
//! [`Backend`] is the seam between frame input prep (capture + color conversion,
//! owned by [`Encoder`](super::Encoder)) and the codec itself. Every backend
//! takes a planar I420 [`Frame`] and emits **Annex-B** H.264 with in-band
//! SPS/PPS, ready for `moq_mux::codec::h264::Import` in avc3 mode. Keeping a
//! single wire format means the producer and its on-demand catalog logic don't
//! care which encoder is running.
//!
//! [`open`] picks the best backend for a [`Kind`](super::Kind), trying hardware
//! candidates (platform-gated) before the openh264 software fallback.

use bytes::Bytes;

use super::encoder::{Config, Kind};
use crate::Error;
use crate::frame::Frame;

mod openh264;

#[cfg(target_os = "macos")]
mod videotoolbox;

#[cfg(all(target_os = "linux", feature = "nvenc"))]
mod nvenc;

#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi;

/// An opened H.264 encoder. Feed it frames at the configured resolution;
/// get back zero or more Annex-B H.264 packets.
pub(crate) trait Backend: Send {
	/// Encode one frame. Set `keyframe` to force an IDR (e.g. on resume so a
	/// re-subscribing viewer can start decoding at once).
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>;

	/// Flush the encoder, returning any buffered packets.
	fn finish(&mut self) -> Result<Vec<Bytes>, Error>;

	/// The encoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}

/// A backend constructor: name plus an opener that tries to start it.
struct Candidate {
	name: &'static str,
	open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
	/// Needs a dmabuf-backed [`Frame`] (VAAPI). Such backends are excluded from
	/// `Auto`/`Hardware` selection and reachable only by name: a box compiled
	/// with `vaapi` but lacking a VAAPI device must still fall back to the CPU
	/// webcam + software/NVENC path rather than capture dmabuf no encoder here
	/// can use. When chosen by name, the caller opens the matching V4L2 dmabuf
	/// capture (see [`requires_dmabuf`] and `capture::open`'s `want_dmabuf`).
	requires_dmabuf: bool,
}

/// Whether selecting `kind` will require dmabuf-backed frames, so the caller can
/// open the zero-copy V4L2 capture to match. Only an explicit by-name choice of
/// a dmabuf backend qualifies; `Auto`/`Hardware`/`Software` stay on CPU frames.
pub(crate) fn requires_dmabuf(kind: &Kind) -> bool {
	let Kind::Named(name) = kind else { return false };
	HARDWARE
		.iter()
		.chain(std::iter::once(&SOFTWARE))
		.any(|c| c.name == name && c.requires_dmabuf)
}

/// Hardware backends, in priority order. Platform-gated so only the ones that
/// could plausibly work on this target are even listed.
const HARDWARE: &[Candidate] = &[
	#[cfg(target_os = "macos")]
	Candidate {
		name: videotoolbox::NAME,
		open: videotoolbox::VideoToolbox::open,
		requires_dmabuf: false,
	},
	#[cfg(all(target_os = "linux", feature = "nvenc"))]
	Candidate {
		name: nvenc::NAME,
		open: nvenc::Nvenc::open,
		requires_dmabuf: false,
	},
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	Candidate {
		name: vaapi::NAME,
		open: vaapi::Vaapi::open,
		requires_dmabuf: true,
	},
];

const SOFTWARE: Candidate = Candidate {
	name: openh264::NAME,
	open: openh264::Openh264::open,
	requires_dmabuf: false,
};

/// Open the best encoder for `config.kind`, trying candidates in priority order
/// and falling back until one succeeds.
pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
	// Automatic selection only considers backends whose input we can currently
	// produce; `Kind::Named` opts in explicitly and skips the filter.
	let usable = |c: &&Candidate| !c.requires_dmabuf;
	let candidates: Vec<&Candidate> = match &config.kind {
		Kind::Auto => HARDWARE
			.iter()
			.filter(usable)
			.chain(std::iter::once(&SOFTWARE))
			.collect(),
		Kind::Hardware => HARDWARE.iter().filter(usable).collect(),
		Kind::Software => vec![&SOFTWARE],
		Kind::Named(name) => {
			let all = HARDWARE.iter().chain(std::iter::once(&SOFTWARE));
			all.filter(|c| c.name == name).collect()
		}
	};

	let mut tried = Vec::new();
	for candidate in candidates {
		tried.push(candidate.name);
		match (candidate.open)(config) {
			Ok(backend) => return Ok(backend),
			Err(e) => tracing::debug!(encoder = candidate.name, error = %e, "encoder unavailable, trying next"),
		}
	}

	Err(Error::NoEncoder(tried.join(", ")))
}
