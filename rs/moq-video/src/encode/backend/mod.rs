//! Pluggable video encoder backends.
//!
//! [`Backend`] is the seam between frame input prep (capture + color conversion,
//! owned by [`Encoder`](super::Encoder)) and the codec itself. Every backend
//! takes a raw [`Frame`] and emits Annex-B with in-band parameter sets (SPS/PPS,
//! plus VPS for H.265), the framing the matching catalog importer expects. Each
//! backend produces exactly one codec, so the producer can route its packets to
//! the right importer.
//!
//! Output is stamped with the timestamp of the frame it was encoded from, not
//! whichever frame happened to be going in. A backend that flushes each frame
//! before returning just echoes the input timestamp; one that buffers (Media
//! Foundation) correlates through the codec's own sample clock.
//!
//! [`open`] picks the best backend for a [`Codec`](super::Codec) +
//! [`Kind`](super::Kind): only candidates that support the requested codec are
//! considered, hardware (platform-gated) before the always-available openh264
//! software fallback.

use super::encoder::{Codec, Config, Kind};
use crate::encode::Encoded;
use crate::{Error, Frame};

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
/// back zero or more access units in the codec's wire framing, each stamped with
/// the timestamp of the frame it came from.
pub(crate) trait Backend: Send {
	/// Encode one frame, forcing an IDR when `keyframe` is set. Backends key frames
	/// automatically per [`Config::gop`], so this is only the caller's extra
	/// request, arriving via [`Encoder::keyframe`](super::Encoder::keyframe).
	fn encode(&mut self, frame: &Frame, keyframe: bool) -> Result<Vec<Encoded>, Error>;

	/// Return every access unit the codec is still holding, leaving the encoder
	/// usable for the frames that follow.
	///
	/// The caller reaches for this at a boundary the output has to respect, which
	/// on a live track is a group: a codec that pipelines would otherwise carry the
	/// last frames of one group into the next, ahead of its keyframe, where a
	/// consumer joining there cannot decode them.
	///
	/// No default, even though most backends have nothing to hold: a pipelined one
	/// that inherited an empty implementation would drop frames at every boundary
	/// and look like it worked.
	fn flush(&mut self) -> Result<Vec<Encoded>, Error>;

	/// Flush the encoder for the last time, returning any buffered access units.
	fn finish(&mut self) -> Result<Vec<Encoded>, Error>;

	/// Retune the live encoder to `bitrate` bits per second, taking effect from
	/// roughly the next frame. Called as the congestion controller's estimate
	/// moves, so it must not force an IDR or rebuild the session: a keyframe on
	/// every bandwidth change is exactly the burst a closing uplink can't take.
	///
	/// No default: a backend that can't retune has to say so with
	/// [`Error::BitrateUnsupported`](crate::Error::BitrateUnsupported) rather
	/// than inherit a silent no-op and quietly ignore congestion.
	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;

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
		codecs: &[Codec::H264, Codec::H265],
		open: nvenc::Nvenc::open,
	},
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	Candidate {
		name: vaapi::NAME,
		codecs: &[Codec::H264],
		open: vaapi::Vaapi::open,
	},
];

/// Software fallbacks, all platforms, always available so a box with no usable
/// hardware encoder can still encode. Only H.264 (openh264) has one; H.265 is
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

#[cfg(test)]
pub(crate) mod test_util {
	use h264_reader::nal::sps::SeqParameterSet;
	use h264_reader::nal::{Nal, RefNal, UnitType};

	/// A stream's VUI color description, as the raw code points ISO/IEC 23091-2
	/// assigns them plus the range flag.
	///
	/// Deliberately not mapped onto [`Color`](crate::Color): the mapping is lossy
	/// (several code points share a matrix, and BT.709 and SMPTE 170M define the
	/// same transfer curve under different numbers), so a test that compared
	/// `Color`s could not see a backend drift on the fields `Color` folds away.
	#[derive(Debug, PartialEq, Eq)]
	pub(crate) struct Described {
		pub primaries: u8,
		pub transfer: u8,
		pub matrix: u8,
		pub full_range: bool,
	}

	/// The description we emit for BT.601 and BT.709 limited range, shared by the
	/// backend tests so a backend drifting from the others fails rather than
	/// quietly encoding its own dialect.
	///
	/// BT.601 goes out as SMPTE 170M primaries and matrix (code point 6) with the
	/// BT.709 transfer curve (1). The two curves are defined identically, and
	/// CoreVideo's SMPTE 170M transfer constant is deprecated while Media
	/// Foundation has none at all, so 1 is the only value all four backends can
	/// actually emit.
	pub(crate) const BT601_DESCRIBED: Described = Described {
		primaries: 6,
		transfer: 1,
		matrix: 6,
		full_range: false,
	};

	pub(crate) const BT709_DESCRIBED: Described = Described {
		primaries: 1,
		transfer: 1,
		matrix: 1,
		full_range: false,
	};

	/// The color description an H.264 Annex-B stream carries in its SPS, or `None`
	/// if it carries none and a decoder would have to guess.
	///
	/// Reads the bitstream rather than the encoder's config, so a backend that
	/// quietly drops the VUI (or a driver that ignores it) fails the test instead
	/// of passing on our own bookkeeping.
	pub(crate) fn declared_color(annexb: &[u8]) -> Option<Described> {
		// Every 4-byte start code contains a 3-byte one at offset 1, so scanning
		// for the short form finds both.
		let starts: Vec<usize> = (0..annexb.len().saturating_sub(2))
			.filter(|&i| annexb[i..i + 3] == [0, 0, 1])
			.map(|i| i + 3)
			.collect();

		let sps = starts.iter().enumerate().find_map(|(n, &start)| {
			// Bound the NAL at the next start code: a trailing slice would
			// leave the SPS parser reading into the following NAL.
			let end = starts.get(n + 1).map_or(annexb.len(), |&next| next - 3);
			let nal = RefNal::new(&annexb[start..end], &[], true);
			match nal.header().ok()?.nal_unit_type() {
				UnitType::SeqParameterSet => SeqParameterSet::from_bits(nal.rbsp_bits()).ok(),
				_ => None,
			}
		})?;

		let signal = sps.vui_parameters.as_ref()?.video_signal_type.as_ref()?;
		let description = signal.colour_description.as_ref()?;
		Some(Described {
			primaries: description.colour_primaries,
			transfer: description.transfer_characteristics,
			matrix: description.matrix_coefficients,
			full_range: signal.video_full_range_flag,
		})
	}
}
