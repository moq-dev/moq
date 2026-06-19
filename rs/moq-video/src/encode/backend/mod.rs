//! Pluggable video encoder backends.
//!
//! [`Backend`] is the seam between frame input prep (capture + color conversion,
//! owned by [`Encoder`](super::Encoder)) and the codec itself. Every backend
//! takes a planar I420 [`Frame`] and emits Annex-B with in-band parameter sets
//! (SPS/PPS, plus VPS for H.265), the framing the matching catalog importer
//! expects. Each backend produces exactly one codec, so the producer can route
//! its packets to the right importer.
//!
//! [`open`] picks the best backend for a [`Codec`](super::Codec) +
//! [`Kind`](super::Kind): only candidates that support the requested codec are
//! considered, hardware (platform-gated) before software.

use bytes::Bytes;

use super::encoder::{Codec, Config, Kind};
use crate::Error;
use crate::frame::Frame;

mod openh264;

#[cfg(target_os = "macos")]
mod videotoolbox;

#[cfg(target_os = "windows")]
mod mediafoundation;

#[cfg(all(target_os = "linux", feature = "nvenc"))]
mod nvenc;

#[cfg(all(target_os = "linux", feature = "vaapi"))]
mod vaapi;

/// An opened video encoder. Feed it frames at the configured resolution; get
/// back zero or more packets in the codec's wire framing.
pub(crate) trait Backend: Send {
	/// Encode one frame. Set `keyframe` to force an IDR (e.g. on resume so a
	/// re-subscribing viewer can start decoding at once).
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Bytes>, Error>;

	/// Flush the encoder, returning any buffered packets.
	fn finish(&mut self) -> Result<Vec<Bytes>, Error>;

	/// The encoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}

/// A backend constructor: name, the codecs it can emit, and an opener.
struct Candidate {
	name: &'static str,
	codecs: &'static [Codec],
	open: fn(&Config) -> Result<Box<dyn Backend>, Error>,
}

/// Hardware backends, in priority order. Platform-gated so only the ones that
/// could plausibly work on this target are even listed.
const HARDWARE: &[Candidate] = &[
	#[cfg(target_os = "macos")]
	Candidate {
		name: videotoolbox::NAME,
		codecs: &[Codec::H264, Codec::H265],
		open: videotoolbox::VideoToolbox::open,
	},
	#[cfg(target_os = "windows")]
	Candidate {
		name: mediafoundation::NAME,
		codecs: &[Codec::H264, Codec::H265],
		open: mediafoundation::MediaFoundation::open,
	},
	#[cfg(all(target_os = "linux", feature = "nvenc"))]
	Candidate {
		name: nvenc::NAME,
		codecs: &[Codec::H264],
		open: nvenc::Nvenc::open,
	},
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	Candidate {
		name: vaapi::NAME,
		codecs: &[Codec::H264],
		open: vaapi::Vaapi::open,
	},
];

/// Software fallbacks, all platforms. Only H.264 (openh264) has one; H.265 is
/// hardware-only. A slice so future software codecs slot in.
const SOFTWARE: &[Candidate] = &[Candidate {
	name: openh264::NAME,
	codecs: &[Codec::H264],
	open: openh264::Openh264::open,
}];

/// Open the best encoder for `config.codec` + `config.kind`, trying candidates
/// in priority order and falling back until one succeeds.
pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
	let codec = config.codec;
	let supports = move |c: &&Candidate| c.codecs.contains(&codec);
	let hardware = HARDWARE.iter().filter(supports);
	let software = SOFTWARE.iter().filter(supports);

	let candidates: Vec<&Candidate> = match &config.kind {
		Kind::Auto => hardware.chain(software).collect(),
		Kind::Hardware => hardware.collect(),
		Kind::Software => software.collect(),
		Kind::Named(name) => HARDWARE
			.iter()
			.chain(SOFTWARE.iter())
			.filter(supports)
			.filter(|c| c.name == name)
			.collect(),
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
