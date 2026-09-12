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
//! / DXVA on Windows, MediaCodec on Android, NVDEC, VAAPI, then V4L2 on Linux) before
//! the openh264 software fallback, exactly like the encode side. Only backends
//! that support the requested codec are considered: there is no software H.265
//! or AV1 decoder, so those tracks have no fallback below the hardware path.

use bytes::Bytes;
use moq_net::Timestamp;

use super::decoder::{Config, Kind};
use crate::{Error, Frame};

mod openh264;

#[cfg(test)]
pub(crate) mod probe;

#[cfg(target_os = "macos")]
mod videotoolbox;

#[cfg(target_os = "windows")]
mod mediafoundation;

#[cfg(all(target_os = "android", feature = "mediacodec"))]
mod mediacodec;

#[cfg(all(target_os = "linux", feature = "nvidia"))]
mod nvdec;

// Crate-visible so `decode::Consumer`'s end-of-track test can name the one
// backend that holds pictures back; nothing outside the tests reaches for it.
#[cfg(all(target_os = "linux", feature = "vaapi"))]
pub(crate) mod vaapi;

#[cfg(all(target_os = "linux", feature = "v4l2"))]
mod v4l2;

/// The video codec a decoder handles, derived from the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Codec {
	/// H.264 / AVC video.
	H264,
	/// H.265 / HEVC video.
	H265,
	/// AV1 video.
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

/// An opened decoder. Feed it prepared access units in decode order; get back
/// zero or more decoded frames (zero while the decoder is still buffering, e.g.
/// before the first keyframe's parameter sets).
pub(crate) trait Backend: Send {
	/// Decode one access unit stamped with its presentation `timestamp`.
	/// `keyframe` marks a random-access frame. Takes an owned [`Bytes`] so a
	/// backend can split codec units without copying.
	/// Backends that decode one-in one-out echo the input timestamp; NVDEC and
	/// MediaCodec thread timestamps through the codec, so they survive decoder
	/// delay and frame reordering.
	fn decode(&mut self, access_unit: Bytes, timestamp: Timestamp, keyframe: bool) -> Result<Vec<Frame>, Error>;

	/// Return the pictures the codec still holds once the stream has ended.
	///
	/// Backends configured for zero delay return no frames. A backend that
	/// reorders pictures overrides this so the end of a track does not drop its
	/// buffered tail.
	fn flush(&mut self) -> Result<Vec<Frame>, Error>;

	/// The decoder name in use, e.g. `"videotoolbox"` (for logging).
	fn name(&self) -> &str;
}

/// Every decoder backend this crate has a name for, on any platform.
///
/// The decode counterpart of [`encode::backend::NAMES`](crate::encode::NAMES),
/// and platform-complete for the same reason.
pub const NAMES: &[&str] = &[
	"videotoolbox",
	"mediafoundation",
	"mediacodec",
	"nvdec",
	"vaapi",
	"v4l2",
	"openh264",
];

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
	#[cfg(all(target_os = "android", feature = "mediacodec"))]
	Candidate {
		name: mediacodec::NAME,
		supports: |c| matches!(c, Codec::H264 | Codec::H265 | Codec::Av1),
		open: mediacodec::MediaCodec::open,
	},
	#[cfg(all(target_os = "linux", feature = "nvidia"))]
	Candidate {
		name: nvdec::NAME,
		supports: |c| matches!(c, Codec::H264 | Codec::H265 | Codec::Av1),
		open: nvdec::Nvdec::open,
	},
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	Candidate {
		name: vaapi::NAME,
		supports: |c| matches!(c, Codec::H264),
		open: vaapi::Vaapi::open,
	},
	// Last of the Linux hardware decoders, for the same reason as its encode
	// counterpart: the SoC blocks it drives are the only hardware on a board that
	// has no NVIDIA GPU.
	#[cfg(all(target_os = "linux", feature = "v4l2"))]
	Candidate {
		name: v4l2::NAME,
		supports: |c| matches!(c, Codec::H264),
		open: v4l2::V4l2::open,
	},
];

const SOFTWARE: Candidate = Candidate {
	name: openh264::NAME,
	supports: |c| matches!(c, Codec::H264),
	open: openh264::Openh264::open,
};

/// Test-only backends. Deliberately in neither list above, so `Auto` /
/// `Hardware` / `Software` can never select one: they exist to be asked for by
/// name.
#[cfg(test)]
const NAMED_ONLY: &[Candidate] = &[
	Candidate {
		name: probe::NAME,
		supports: |c| matches!(c, Codec::H264),
		open: probe::Probe::open,
	},
	Candidate {
		name: probe::BUFFERED_NAME,
		supports: |c| matches!(c, Codec::H264),
		open: probe::Buffered::open,
	},
	#[cfg(not(target_os = "macos"))]
	Candidate {
		name: probe::BLOCKING_FLUSH_NAME,
		supports: |c| matches!(c, Codec::H264),
		open: probe::BlockingFlush::open,
	},
];

#[cfg(not(test))]
const NAMED_ONLY: &[Candidate] = &[];

/// A candidate paired with the tier it came from, so [`select`] can tell a
/// software decoder that was asked for from one reached by falling past
/// hardware that refused to open.
struct Attempt<'a> {
	candidate: &'a Candidate,
	hardware: bool,
}

impl<'a> Attempt<'a> {
	fn hardware(candidate: &'a Candidate) -> Self {
		Self {
			candidate,
			hardware: true,
		}
	}

	fn software(candidate: &'a Candidate) -> Self {
		Self {
			candidate,
			hardware: false,
		}
	}
}

/// Open the best decoder for `codec` and `config`, trying candidates in priority
/// order and falling back until one succeeds. Candidates that don't support the
/// codec are skipped before they're even tried.
pub(crate) fn open(codec: Codec, config: &Config) -> Result<Box<dyn Backend>, Error> {
	let attempts: Vec<Attempt> = match &config.kind {
		Kind::Auto => HARDWARE
			.iter()
			.map(Attempt::hardware)
			.chain(std::iter::once(Attempt::software(&SOFTWARE)))
			.collect(),
		Kind::Hardware => HARDWARE.iter().map(Attempt::hardware).collect(),
		Kind::Software => vec![Attempt::software(&SOFTWARE)],
		Kind::Named(name) => HARDWARE
			.iter()
			.map(Attempt::hardware)
			.chain(
				std::iter::once(&SOFTWARE)
					.chain(NAMED_ONLY.iter())
					.map(Attempt::software),
			)
			.filter(|a| a.candidate.name == name)
			.collect(),
	};

	select(codec, attempts, config)
}

/// Try `attempts` in order and return the first decoder that opens, warning when
/// that means falling past hardware.
///
/// Split out from [`open`] for the same reason as its encode counterpart: the
/// candidate lists are platform-gated consts, so a test supplies its own
/// attempts rather than depending on what the host GPU can do.
fn select(codec: Codec, attempts: Vec<Attempt>, config: &Config) -> Result<Box<dyn Backend>, Error> {
	// Each entry is "name: why it refused". The names alone say which backends
	// exist, which is what a reader already knows; the reasons say why this
	// machine has none, which is the question being asked.
	let mut tried: Vec<String> = Vec::new();
	let mut refused = Vec::new();

	for attempt in attempts {
		if !(attempt.candidate.supports)(codec) {
			continue;
		}

		let name = attempt.candidate.name;

		match (attempt.candidate.open)(codec, config) {
			Ok(backend) => {
				// Same reasoning as the encode side: a compiled-in hardware decoder that
				// refuses to open is otherwise invisible, since `Auto` hands back a
				// working software decoder and says nothing above DEBUG.
				if !attempt.hardware && !refused.is_empty() {
					tracing::warn!(
						decoder = name,
						refused = %refused.join(", "),
						"no hardware decoder available, falling back to software"
					);
				}
				return Ok(backend);
			}
			Err(e) => {
				tracing::debug!(decoder = name, error = %e, "decoder unavailable, trying next");
				tried.push(format!("{name}: {e}"));
				if attempt.hardware {
					refused.push(format!("{name}: {e}"));
				}
			}
		}
	}

	// Nothing was tried at all, so no candidate both matched and takes this
	// codec. For a named request that is a name this build does not have: a
	// typo, a feature that is off, or a backend that does not decode this
	// codec. Naming what is here is most of the answer.
	if tried.is_empty() {
		let available = available_names(codec);
		return match &config.kind {
			Kind::Named(name) => Err(Error::UnknownDecoder {
				name: name.clone(),
				codec,
				available: available.join(", "),
			}),
			kind => Err(Error::NoDecoder(format!(
				"nothing compiled in for {} at {kind:?} (this build has: {})",
				codec.label(),
				available.join(", "),
			))),
		};
	}
	Err(Error::NoDecoder(tried.join(", ")))
}

/// Returns the decoders this build has for `codec`, in priority order.
///
/// Only the ones a user could ask for: the test-only list is left out, since it
/// exists to be named by a test rather than offered to anybody.
fn available_names(codec: Codec) -> Vec<&'static str> {
	HARDWARE
		.iter()
		.chain(std::iter::once(&SOFTWARE))
		.filter(|candidate| (candidate.supports)(codec))
		.map(|candidate| candidate.name)
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A backend that opens and decodes nothing, the decode mirror of the encode
	/// side's stub.
	struct Stub;

	impl Stub {
		fn open(_codec: Codec, _config: &Config) -> Result<Box<dyn Backend>, Error> {
			Ok(Box::new(Self))
		}
	}

	impl Backend for Stub {
		fn decode(&mut self, _access_unit: Bytes, _timestamp: Timestamp, _keyframe: bool) -> Result<Vec<Frame>, Error> {
			Ok(Vec::new())
		}

		fn flush(&mut self) -> Result<Vec<Frame>, Error> {
			Ok(Vec::new())
		}

		fn name(&self) -> &str {
			"stub"
		}
	}

	const WORKING: Candidate = Candidate {
		name: "stub",
		supports: |c| matches!(c, Codec::H264),
		open: Stub::open,
	};

	/// Compiled in but refusing at runtime, the way NVDEC does on a host whose
	/// driver libraries aren't on the loader path.
	const REFUSING: Candidate = Candidate {
		name: "driverless",
		supports: |c| matches!(c, Codec::H264),
		open: |_, _| Err(Error::Codec(anyhow::anyhow!("driver libraries not found"))),
	};

	#[tracing_test::traced_test]
	#[test]
	fn falling_past_hardware_warns() {
		let config = Config::new();
		let attempts = vec![Attempt::hardware(&REFUSING), Attempt::software(&WORKING)];
		let backend = select(Codec::H264, attempts, &config).unwrap();
		assert_eq!(backend.name(), "stub");

		logs_assert(
			|lines: &[&str]| match lines.iter().find(|line| line.contains("falling back to software")) {
				Some(warning) if warning.contains("driverless") && warning.contains("driver libraries not found") => {
					Ok(())
				}
				Some(warning) => Err(format!("warning does not name the refusal: {warning}")),
				None => Err("no fallback warning".to_owned()),
			},
		);
	}

	/// A hardware candidate skipped for not supporting the codec never ran, so it
	/// refused nothing and the software pick isn't a fallback. Only the decode side
	/// can hit this: it filters by codec inside the loop rather than up front.
	#[tracing_test::traced_test]
	#[test]
	fn hardware_that_cannot_decode_the_codec_is_not_a_fallback() {
		const H265_ONLY: Candidate = Candidate {
			name: "driverless",
			supports: |c| matches!(c, Codec::H265),
			open: |_, _| Err(Error::Codec(anyhow::anyhow!("driver libraries not found"))),
		};

		let attempts = vec![Attempt::hardware(&H265_ONLY), Attempt::software(&WORKING)];
		select(Codec::H264, attempts, &Config::new()).unwrap();
		assert!(!logs_contain("no hardware decoder available"));
	}

	/// A name no candidate answers to has to say so, and say what it could have
	/// been asked for instead.
	#[test]
	fn an_unknown_name_names_itself_and_the_alternatives() {
		let mut config = Config::new();
		config.kind = Kind::Named("vappi".to_owned());

		match open(Codec::H264, &config) {
			Err(Error::UnknownDecoder { name, codec, available }) => {
				assert_eq!(name, "vappi");
				assert_eq!(codec, crate::decode::Codec::H264);
				// openh264 is unconditional, so every build has one to offer.
				assert!(available.contains(openh264::NAME), "nothing offered: {available}");
			}
			Err(other) => panic!("expected UnknownDecoder, got {other:?}"),
			Ok(backend) => panic!("expected UnknownDecoder, opened {}", backend.name()),
		}
	}

	/// The reason each candidate refused belongs in the error. Only the DEBUG
	/// line used to carry it, which is no use to a caller holding the `Err`.
	#[test]
	fn every_candidate_refusing_reports_why() {
		let mut config = Config::new();
		config.kind = Kind::Named("driverless".to_owned());

		match select(Codec::H264, vec![Attempt::hardware(&REFUSING)], &config) {
			Err(Error::NoDecoder(tried)) => {
				assert!(tried.contains("driverless"), "does not name the backend: {tried}");
				assert!(
					tried.contains("driver libraries not found"),
					"does not carry the reason: {tried}"
				);
			}
			Err(other) => panic!("expected NoDecoder, got {other:?}"),
			Ok(backend) => panic!("expected NoDecoder, opened {}", backend.name()),
		}
	}

	/// The decode half of the same guarantee the encode side keeps.
	#[test]
	fn every_compiled_backend_is_named_publicly() {
		for candidate in HARDWARE.iter().chain(std::iter::once(&SOFTWARE)) {
			assert!(
				NAMES.contains(&candidate.name),
				"{} is compiled in but missing from NAMES",
				candidate.name,
			);
		}
	}
}
