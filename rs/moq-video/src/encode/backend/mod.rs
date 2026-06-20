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
//! candidates (platform-gated) before the openh264 software fallback, which is
//! only compiled in when the `software` feature is enabled.

use bytes::Bytes;

use super::encoder::{Config, Kind};
use crate::Error;
use crate::frame::Frame;

#[cfg(feature = "software")]
mod openh264;

#[cfg(target_os = "macos")]
mod videotoolbox;

#[cfg(target_os = "windows")]
mod mediafoundation;

#[cfg(target_os = "linux")]
mod nvenc;

#[cfg(target_os = "linux")]
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
}

/// Hardware backends, in priority order. Platform-gated so only the ones that
/// could plausibly work on this target are even listed.
const HARDWARE: &[Candidate] = &[
	#[cfg(target_os = "macos")]
	Candidate {
		name: videotoolbox::NAME,
		open: videotoolbox::VideoToolbox::open,
	},
	#[cfg(target_os = "windows")]
	Candidate {
		name: mediafoundation::NAME,
		open: mediafoundation::MediaFoundation::open,
	},
	#[cfg(target_os = "linux")]
	Candidate {
		name: nvenc::NAME,
		open: nvenc::Nvenc::open,
	},
	#[cfg(target_os = "linux")]
	Candidate {
		name: vaapi::NAME,
		open: vaapi::Vaapi::open,
	},
];

/// The software fallback, only compiled in with the `software` feature.
#[cfg(feature = "software")]
const SOFTWARE: Candidate = Candidate {
	name: openh264::NAME,
	open: openh264::Openh264::open,
};

/// Open the best encoder for `config.kind`, trying candidates in priority order
/// and falling back until one succeeds. The software fallback only participates
/// when the `software` feature is enabled; without it, software-only requests
/// (and `Auto` on a box with no hardware encoder) yield [`Error::NoEncoder`].
pub(crate) fn open(config: &Config) -> Result<Box<dyn Backend>, Error> {
	let candidates: Vec<&Candidate> = match &config.kind {
		Kind::Auto => {
			#[cfg_attr(not(feature = "software"), allow(unused_mut))]
			let mut c: Vec<&Candidate> = HARDWARE.iter().collect();
			#[cfg(feature = "software")]
			c.push(&SOFTWARE);
			c
		}
		Kind::Hardware => HARDWARE.iter().collect(),
		Kind::Software => {
			#[cfg(feature = "software")]
			let c = vec![&SOFTWARE];
			#[cfg(not(feature = "software"))]
			let c: Vec<&Candidate> = Vec::new();
			c
		}
		Kind::Named(name) => {
			#[cfg_attr(not(feature = "software"), allow(unused_mut))]
			let mut c: Vec<&Candidate> = HARDWARE.iter().filter(|c| c.name == name).collect();
			#[cfg(feature = "software")]
			if SOFTWARE.name == name {
				c.push(&SOFTWARE);
			}
			c
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
