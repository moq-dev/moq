//! Pluggable video decoder backends.
//!
//! The mirror of [`encode::backend`](crate::encode). [`Backend`] is the seam
//! between the access-unit prep (keyframe gating plus any codec-specific payload
//! conversion, owned by [`Decoder`](super::Decoder)) and the codec itself. H.264
//! / H.265 backends take Annex-B access units with parameter sets inline ahead
//! of each keyframe; AV1 backends take OBU temporal units directly.
//!
//! [`open`] picks the best backend for a [`Codec`] and [`Config`], trying
//! hardware candidates (platform-gated: VideoToolbox on macOS and iOS, Media Foundation
//! / DXVA on Windows, MediaCodec on Android, NVDEC, VAAPI, then V4L2 on Linux) before
//! the software decoders this build enables (OpenH264 for H.264, libvpx for VP8
//! and VP9), exactly like the encode side. Only backends that support the
//! requested codec are considered: there is no software H.265 or AV1 decoder, so
//! those tracks have no fallback below the hardware path. VP8 / VP9 backends
//! take one coded frame per call.
//!
//! A backend that fails to open either refuses the [`Stream`] or faults. A
//! refusal is [`Error::UnsupportedCodec`]: the device answered that it cannot
//! decode this stream, so no retry on this machine helps but another rendition
//! may. Anything else is a fault: missing drivers, no device, or a library
//! error. A backend that cannot tell the two apart reports a fault.

use bytes::Bytes;
use moq_net::Timestamp;

use super::decoder::{Config, Kind};
use crate::{Error, Frame, Size};

#[cfg(feature = "openh264")]
mod openh264;

#[cfg(test)]
pub(crate) mod probe;

#[cfg(feature = "vpx")]
mod vpx;

#[cfg(apple)]
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
	/// VP8 video.
	Vp8,
	/// VP9 video.
	Vp9,
}

impl Codec {
	fn label(self) -> &'static str {
		match self {
			Codec::H264 => "H.264",
			Codec::H265 => "H.265",
			Codec::Av1 => "AV1",
			Codec::Vp8 => "VP8",
			Codec::Vp9 => "VP9",
		}
	}
}

/// The stream a decoder opens for, as far as the catalog describes it before
/// the first picture arrives.
///
/// What a backend needs to ask its device whether it decodes the stream at
/// all. The catalog is the peer's claim, so a stream that turns out otherwise
/// still fails at its first keyframe.
#[derive(Clone, Debug)]
pub(crate) struct Stream {
	pub codec: Codec,
	/// The H.264 `profile_idc` the codec string declares.
	#[cfg_attr(
		not(all(target_os = "linux", feature = "vaapi")),
		allow(dead_code, reason = "only VA-API asks its driver by profile")
	)]
	pub h264_profile: Option<u8>,
	/// The coded size, when the description's parameter sets pin it. Without
	/// them the catalog's coded size is only a ceiling the picture resizes
	/// below in band, so it says nothing a device could refuse.
	#[cfg_attr(
		not(all(target_os = "linux", feature = "nvidia")),
		allow(dead_code, reason = "only NVDEC checks a size before the first keyframe")
	)]
	pub size: Option<Size>,
	/// Annex-B parameter sets from the description, for a track that carries
	/// them out of band.
	#[cfg_attr(
		not(apple),
		allow(dead_code, reason = "only VideoToolbox builds its session from them")
	)]
	pub parameter_sets: Option<Bytes>,
}

impl From<Codec> for Stream {
	/// A stream of `codec` the catalog says nothing more about.
	fn from(codec: Codec) -> Self {
		Self {
			codec,
			h264_profile: None,
			size: None,
			parameter_sets: None,
		}
	}
}

/// An opened decoder. Feed it prepared access units in decode order; get back
/// zero or more decoded frames (zero while the decoder is still buffering, e.g.
/// before the first keyframe's parameter sets).
pub(crate) trait Backend {
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
	"vpx",
];

/// A backend opener: builds a decoder for a stream and config, refusing a
/// stream its device cannot decode with [`Error::UnsupportedCodec`].
type Open = fn(&Stream, &Config) -> Result<Box<dyn Backend>, Error>;

/// A backend constructor: name, the codecs it can decode, and an opener.
struct Candidate {
	name: &'static str,
	supports: fn(Codec) -> bool,
	open: Open,
}

/// Hardware backends, in priority order. Platform-gated so only the ones that
/// could plausibly work on this target are even listed.
const HARDWARE: &[Candidate] = &[
	#[cfg(apple)]
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

const SOFTWARE: &[Candidate] = &[
	#[cfg(feature = "openh264")]
	Candidate {
		name: openh264::NAME,
		supports: |c| matches!(c, Codec::H264),
		open: openh264::Openh264::open,
	},
	#[cfg(feature = "vpx")]
	Candidate {
		name: vpx::NAME,
		supports: |c| matches!(c, Codec::Vp8 | Codec::Vp9),
		open: vpx::Vpx::open,
	},
];

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
		supports: |c| matches!(c, Codec::H264 | Codec::H265 | Codec::Vp9),
		open: probe::Buffered::open,
	},
	Candidate {
		name: probe::IMMEDIATE_NAME,
		supports: |c| matches!(c, Codec::H264),
		open: probe::Immediate::open,
	},
	Candidate {
		name: probe::SINGLE_NAME,
		supports: |c| matches!(c, Codec::H264),
		open: probe::Single::open,
	},
	Candidate {
		name: probe::NATIVE_NAME,
		supports: |c| matches!(c, Codec::H264),
		open: probe::Native::open,
	},
	Candidate {
		name: probe::REFUSING_NAME,
		supports: |c| matches!(c, Codec::H264),
		open: probe::Refusing::open,
	},
	#[cfg(not(apple))]
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
/// hardware that failed to open.
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
pub(crate) fn open(stream: &Stream, config: &Config) -> Result<Box<dyn Backend>, Error> {
	let attempts: Vec<Attempt> = match &config.kind {
		Kind::Auto => HARDWARE
			.iter()
			.map(Attempt::hardware)
			.chain(SOFTWARE.iter().map(Attempt::software))
			.collect(),
		Kind::Hardware => HARDWARE.iter().map(Attempt::hardware).collect(),
		Kind::Software => SOFTWARE.iter().map(Attempt::software).collect(),
		Kind::Named(name) => HARDWARE
			.iter()
			.map(Attempt::hardware)
			.chain(SOFTWARE.iter().chain(NAMED_ONLY.iter()).map(Attempt::software))
			.filter(|a| a.candidate.name == name)
			.collect(),
	};

	select(stream, attempts, config)
}

/// Try `attempts` in order and return the first decoder that opens, warning when
/// that means falling past hardware.
///
/// Fails with [`Error::UnsupportedCodec`] only when every candidate that takes
/// the codec refused the stream: one fault among them could be what stopped a
/// decoder that would have worked, so the whole is a fault.
///
/// Split out from [`open`] for the same reason as its encode counterpart: the
/// candidate lists are platform-gated consts, so a test supplies its own
/// attempts rather than depending on what the host GPU can do.
fn select(stream: &Stream, attempts: Vec<Attempt>, config: &Config) -> Result<Box<dyn Backend>, Error> {
	let codec = stream.codec;
	// Each entry is "name: why it failed". The names alone say which backends
	// exist, which is what a reader already knows; the reasons say why this
	// machine has none, which is the question being asked.
	let mut tried: Vec<String> = Vec::new();
	let mut skipped = Vec::new();
	let mut faulted = false;

	for attempt in attempts {
		if !(attempt.candidate.supports)(codec) {
			continue;
		}

		let name = attempt.candidate.name;

		match (attempt.candidate.open)(stream, config) {
			Ok(backend) => {
				// Same reasoning as the encode side: a compiled-in hardware decoder that
				// fails to open is otherwise invisible, since `Auto` hands back a
				// working software decoder and says nothing above DEBUG.
				if !attempt.hardware && !skipped.is_empty() {
					tracing::warn!(
						decoder = name,
						skipped = %skipped.join(", "),
						"no hardware decoder available, falling back to software"
					);
				}
				return Ok(backend);
			}
			Err(e) => {
				tracing::debug!(decoder = name, error = %e, "decoder unavailable, trying next");
				// The one variant a backend opens with to say its device answered.
				faulted |= !matches!(e, Error::UnsupportedCodec(_));
				let reason = match e {
					Error::UnsupportedCodec(reason) => reason,
					e => e.to_string(),
				};
				tried.push(format!("{name}: {reason}"));
				if attempt.hardware {
					skipped.push(format!("{name}: {reason}"));
				}
			}
		}
	}

	// Nothing was tried at all, so no candidate both matched and takes this
	// codec. For a named request that is a name this build does not have: a
	// typo, a feature that is off, or a backend that does not decode this
	// codec. Naming what is here is most of the answer. Otherwise this build
	// cannot decode the codec at all, which is known before opening anything.
	if tried.is_empty() {
		let available = available_names(codec);
		return match &config.kind {
			Kind::Named(name) => Err(Error::UnknownDecoder {
				name: name.clone(),
				codec,
				available: available.join(", "),
			}),
			kind => Err(Error::UnsupportedCodec(format!(
				"nothing compiled in for {} at {kind:?} (this build has: {})",
				codec.label(),
				available.join(", "),
			))),
		};
	}
	if !faulted {
		return Err(Error::UnsupportedCodec(format!(
			"no decoder here takes this {} stream ({})",
			codec.label(),
			tried.join(", ")
		)));
	}
	// Missing drivers, no device, or a library fault: not a property of the track.
	Err(Error::NoDecoder(tried.join(", ")))
}

/// Returns the decoders this build has for `codec`, in priority order.
///
/// Only the ones a user could ask for: the test-only list is left out, since it
/// exists to be named by a test rather than offered to anybody.
fn available_names(codec: Codec) -> Vec<&'static str> {
	HARDWARE
		.iter()
		.chain(SOFTWARE.iter())
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
		fn open(_stream: &Stream, _config: &Config) -> Result<Box<dyn Backend>, Error> {
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

	/// Compiled in but faulting at runtime, the way NVDEC does on a host whose
	/// driver libraries aren't on the loader path.
	const DRIVERLESS: Candidate = Candidate {
		name: "driverless",
		supports: |c| matches!(c, Codec::H264),
		open: |_, _| Err(Error::Codec(anyhow::anyhow!("driver libraries not found"))),
	};

	#[tracing_test::traced_test]
	#[test]
	fn falling_past_hardware_warns() {
		let config = Config::new();
		let attempts = vec![Attempt::hardware(&DRIVERLESS), Attempt::software(&WORKING)];
		let backend = select(&Codec::H264.into(), attempts, &config).unwrap();
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
		select(&Codec::H264.into(), attempts, &Config::new()).unwrap();
		assert!(!logs_contain("no hardware decoder available"));
	}

	/// A name no candidate answers to has to say so, and say what it could have
	/// been asked for instead.
	#[test]
	fn an_unknown_name_names_itself_and_the_alternatives() {
		let mut config = Config::new();
		config.kind = Kind::Named("vappi".to_owned());

		match open(&Codec::H264.into(), &config) {
			Err(Error::UnknownDecoder { name, codec, available }) => {
				assert_eq!(name, "vappi");
				assert_eq!(codec, crate::decode::Codec::H264);
				#[cfg(feature = "openh264")]
				assert!(available.contains(openh264::NAME), "nothing offered: {available}");
				#[cfg(not(feature = "openh264"))]
				assert!(!available.contains("openh264"), "disabled backend offered: {available}");
			}
			Err(other) => panic!("expected UnknownDecoder, got {other:?}"),
			Ok(backend) => panic!("expected UnknownDecoder, opened {}", backend.name()),
		}
	}

	#[cfg(not(feature = "openh264"))]
	#[test]
	fn disabled_software_backend_is_not_selected() {
		let mut config = Config::new();
		config.kind = Kind::Software;
		assert!(matches!(
			open(&Codec::H264.into(), &config),
			Err(Error::UnsupportedCodec(_))
		));

		config.kind = Kind::Named("openh264".to_owned());
		assert!(matches!(
			open(&Codec::H264.into(), &config),
			Err(Error::UnknownDecoder { .. })
		));
	}

	/// The reason each candidate faulted belongs in the error. Only the DEBUG
	/// line used to carry it, which is no use to a caller holding the `Err`.
	#[test]
	fn every_candidate_faulting_reports_why() {
		let mut config = Config::new();
		config.kind = Kind::Named("driverless".to_owned());

		match select(&Codec::H264.into(), vec![Attempt::hardware(&DRIVERLESS)], &config) {
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

	/// The test-only candidate answering to `name`.
	fn named(name: &str) -> &'static Candidate {
		NAMED_ONLY
			.iter()
			.find(|candidate| candidate.name == name)
			.expect("a test-only candidate")
	}

	/// Every candidate's device saying it cannot decode the stream is a property
	/// of the stream on this machine, which a caller can answer with another
	/// rendition.
	#[test]
	fn every_candidate_refusing_is_unsupported() {
		let refusing = named(probe::REFUSING_NAME);
		let attempts = vec![Attempt::hardware(refusing), Attempt::hardware(refusing)];
		match select(&Codec::H264.into(), attempts, &Config::new()) {
			Err(Error::UnsupportedCodec(reason)) => {
				assert!(
					reason.contains(probe::REFUSING_NAME),
					"does not name the backend: {reason}"
				);
				assert!(reason.contains("cannot decode"), "does not carry the reason: {reason}");
			}
			Err(other) => panic!("expected UnsupportedCodec, got {other:?}"),
			Ok(backend) => panic!("expected UnsupportedCodec, opened {}", backend.name()),
		}
	}

	/// A fault among the refusals could be what stopped a decoder that would have
	/// taken the stream, so in either order the whole stays a fault.
	#[test]
	fn a_fault_among_refusals_is_a_fault() {
		let refusing = named(probe::REFUSING_NAME);
		for attempts in [
			vec![Attempt::hardware(refusing), Attempt::hardware(&DRIVERLESS)],
			vec![Attempt::hardware(&DRIVERLESS), Attempt::software(refusing)],
		] {
			match select(&Codec::H264.into(), attempts, &Config::new()) {
				Err(Error::NoDecoder(tried)) => {
					assert!(tried.contains(probe::REFUSING_NAME), "drops the refusal: {tried}");
					assert!(tried.contains("driverless"), "drops the fault: {tried}");
				}
				Err(other) => panic!("expected NoDecoder, got {other:?}"),
				Ok(backend) => panic!("expected NoDecoder, opened {}", backend.name()),
			}
		}
	}

	/// A refusal is no different from any other failure when a later candidate
	/// opens: `Auto` still lands on it.
	#[test]
	fn a_refusal_falls_through_to_the_next_candidate() {
		let attempts = vec![
			Attempt::hardware(named(probe::REFUSING_NAME)),
			Attempt::software(&WORKING),
		];
		let backend = select(&Codec::H264.into(), attempts, &Config::new()).unwrap();
		assert_eq!(backend.name(), "stub");
	}

	/// The decode half of the same guarantee the encode side keeps.
	#[test]
	fn every_compiled_backend_is_named_publicly() {
		for candidate in HARDWARE.iter().chain(SOFTWARE.iter()) {
			assert!(
				NAMES.contains(&candidate.name),
				"{} is compiled in but missing from NAMES",
				candidate.name,
			);
		}
	}
}
