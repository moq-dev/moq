//! Pluggable audio encoder backends.
//!
//! The mirror of the decode backends. [`Backend`] is the seam between the codec
//! and the [`Encoder`](super::Encoder) front end, which owns what every backend
//! of a codec shares: validating [`Settings`](super::Settings) against the
//! codec, framing the input, draining the startup delay at the end, and the
//! catalog entry, whose description is synthesized from the settings rather than
//! read back from the backend.
//!
//! [`open`] tries the platform encoders before the software ones, skipping any
//! that does not emit the codec, and refuses when none opens. [`Kind::Named`]
//! names the codec, not a backend, so which backend encodes it stays the
//! library's choice. The software tier
//! is libopus for Opus and a passthrough for PCM. AAC has no software encoder,
//! so it is only as available as the platform's, and no platform encoder is
//! wired in yet.

use super::Encoded;
use super::encoder::{Codec, Kind, Settings};
use crate::Error;

mod libopus;
mod pcm;

#[cfg(test)]
pub(crate) mod stub;

/// An opened encoder: one frame of interleaved `f32` PCM in, one packet out.
///
/// Input arrives at the settings' rate, in the crate's canonical channel order
/// for the settings' layout; a codec with another native order reorders it here.
///
/// One packet per frame is part of the contract: the producer stamps packets by
/// counting frames. A codec that pipelines output (MediaCodec) needs a `flush`
/// and a zero-or-more return, which changes `Encoder::encode` too, so that lands
/// with the first backend that needs it rather than as an always-empty method.
pub(crate) trait Backend: Send {
	/// Encode exactly one frame of the codec's frame size.
	fn encode(&mut self, pcm: &[f32]) -> Result<Encoded, Error>;

	/// Drop codec history so the next frame codes as if it were the first.
	fn reset(&mut self);

	/// Retune the live encoder to `bitrate` bits per second, a no-op at the current
	/// rate.
	///
	/// No default: a backend that can't change rate mid-stream refuses with
	/// [`Error::Unsupported`] and keeps its opening rate, rather than inheriting a
	/// silent no-op that ignores congestion.
	fn set_bitrate(&mut self, bitrate: u64) -> Result<(), Error>;

	/// The current target bitrate in bits per second, as the codec resolved it.
	fn bitrate(&self) -> u64;

	/// Frames of codec priming at the start of the decoded stream, at the codec
	/// rate: Opus lookahead, AAC encoder delay.
	fn delay(&self) -> usize;

	/// The stable lowercase name of this backend, as [`Encoder::name`](super::Encoder::name)
	/// reports it. Callers never select by it.
	fn name(&self) -> &str;
}

/// A backend constructor: its name, the codecs it emits, and an opener.
struct Candidate {
	name: &'static str,
	codecs: &'static [Codec],
	open: fn(&Settings) -> Result<Box<dyn Backend>, Error>,
}

/// Operating-system encoders, in priority order.
const PLATFORM: &[Candidate] = &[];

const SOFTWARE: &[Candidate] = &[
	Candidate {
		name: libopus::NAME,
		codecs: &[Codec::Opus],
		open: libopus::Libopus::open,
	},
	Candidate {
		name: pcm::NAME,
		codecs: &[Codec::Pcm],
		open: pcm::Pcm::open,
	},
];

/// The platform tier on a test thread that called [`stub::install`], so AAC tests
/// open the same backend on every host and every other test sees the real tiers.
#[cfg(test)]
const STUB_PLATFORM: &[Candidate] = &[Candidate {
	name: stub::NAME,
	codecs: &[Codec::Aac],
	open: stub::Stub::open,
}];

/// Open the first backend that emits the codec and accepts the settings.
pub(crate) fn open(settings: &Settings) -> Result<Box<dyn Backend>, Error> {
	if let Kind::Named(requested) = &settings.kind
		&& requested != settings.codec.as_str()
	{
		return Err(Error::Unsupported(format!(
			"audio encoder {requested:?} is unavailable for {}",
			settings.codec
		)));
	}

	#[cfg(test)]
	let platform = match stub::installed() {
		true => STUB_PLATFORM,
		false => PLATFORM,
	};
	#[cfg(not(test))]
	let platform = PLATFORM;

	select(settings, candidates(&settings.kind, platform, SOFTWARE))
}

/// The candidates `kind` allows, in the order to try them.
///
/// Takes the tiers as arguments so a test can supply stubs instead of whatever
/// this host compiles in.
fn candidates<'a>(kind: &Kind, platform: &'a [Candidate], software: &'a [Candidate]) -> Vec<&'a Candidate> {
	match kind {
		Kind::Software => software.iter().collect(),
		// `open` has already checked that a name matches the codec.
		Kind::Auto | Kind::Named(_) => platform.iter().chain(software).collect(),
	}
}

fn select(settings: &Settings, candidates: Vec<&Candidate>) -> Result<Box<dyn Backend>, Error> {
	let codec = settings.codec;
	let mut refused = Vec::new();

	for candidate in candidates {
		if !candidate.codecs.contains(&codec) {
			continue;
		}
		match (candidate.open)(settings) {
			Ok(backend) => return Ok(backend),
			Err(err) => refused.push((candidate.name, err)),
		}
	}

	// One refusal is the whole answer, so keep its variant.
	if refused.len() == 1 {
		let (_, err) = refused.remove(0);
		return Err(err);
	}
	if !refused.is_empty() {
		let reasons: Vec<String> = refused.iter().map(|(name, err)| format!("{name}: {err}")).collect();
		return Err(Error::Unsupported(reasons.join(", ")));
	}

	let available: Vec<&str> = PLATFORM
		.iter()
		.chain(SOFTWARE)
		.filter(|c| c.codecs.contains(&codec))
		.map(|c| c.name)
		.collect();
	let available = match available.is_empty() {
		true => "none".to_owned(),
		false => available.join(", "),
	};
	Err(Error::Unsupported(match &settings.kind {
		Kind::Software => format!("no software {codec} audio encoder (this build has: {available})"),
		Kind::Auto | Kind::Named(_) => format!("no {codec} audio encoder (this build has: {available})"),
	}))
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Opens anything it advertises and reports which candidate it came from.
	struct Fake(&'static str);

	impl Backend for Fake {
		fn encode(&mut self, _pcm: &[f32]) -> Result<Encoded, Error> {
			Ok(Encoded::new(bytes::Bytes::new()))
		}

		fn reset(&mut self) {}

		fn set_bitrate(&mut self, _bitrate: u64) -> Result<(), Error> {
			Ok(())
		}

		fn bitrate(&self) -> u64 {
			0
		}

		fn delay(&self) -> usize {
			0
		}

		fn name(&self) -> &str {
			self.0
		}
	}

	const PLATFORM_STUB: Candidate = Candidate {
		name: "platform",
		codecs: &[Codec::Aac],
		open: |_| Ok(Box::new(Fake("platform"))),
	};

	/// Compiled in but refusing the settings, like a platform encoder asked for a
	/// layout its framework does not open.
	const REFUSING: Candidate = Candidate {
		name: "refusing",
		codecs: &[Codec::Aac],
		open: |_| Err(Error::Unsupported("not these settings".into())),
	};

	const SOFTWARE_STUB: Candidate = Candidate {
		name: "software",
		codecs: &[Codec::Aac],
		open: |_| Ok(Box::new(Fake("software"))),
	};

	/// Emits nothing but PCM, so an AAC request never reaches its opener.
	const PCM_ONLY: Candidate = Candidate {
		name: "pcm-only",
		codecs: &[Codec::Pcm],
		open: |_| panic!("opened for a codec it does not emit"),
	};

	fn aac(kind: Kind) -> Settings {
		Settings {
			kind,
			..Settings::from_input(Codec::Aac, &Default::default())
		}
	}

	fn pick(kind: Kind, platform: &[Candidate], software: &[Candidate]) -> Result<String, Error> {
		let settings = aac(kind);
		let backend = select(&settings, candidates(&settings.kind, platform, software))?;
		Ok(backend.name().to_owned())
	}

	#[test]
	fn auto_prefers_platform() {
		let name = pick(Kind::Auto, &[PCM_ONLY, PLATFORM_STUB], &[SOFTWARE_STUB]).unwrap();
		assert_eq!(name, "platform");
	}

	#[test]
	fn auto_falls_back_to_software() {
		let name = pick(Kind::Auto, &[REFUSING], &[SOFTWARE_STUB]).unwrap();
		assert_eq!(name, "software");
	}

	#[test]
	fn software_skips_platform() {
		let name = pick(Kind::Software, &[PLATFORM_STUB], &[SOFTWARE_STUB]).unwrap();
		assert_eq!(name, "software");
	}

	/// A codec name is not a backend selector: it tries the same tiers as `Auto`.
	#[test]
	fn named_codec_picks_like_auto() {
		let name = pick(Kind::Named("aac".into()), &[PLATFORM_STUB], &[SOFTWARE_STUB]).unwrap();
		assert_eq!(name, "platform");
	}

	#[test]
	fn every_refusal_is_reported() {
		const ALSO_REFUSING: Candidate = Candidate {
			name: "also-refusing",
			..REFUSING
		};

		let err = pick(Kind::Auto, &[REFUSING], &[ALSO_REFUSING]).unwrap_err();
		let message = err.to_string();
		assert!(
			message.contains("refusing: ") && message.contains("also-refusing: "),
			"{message}"
		);
	}

	/// No AAC encoder is wired in outside the test stub, so `Auto` refuses at
	/// construction and says there is nothing to fall back to. A platform backend
	/// gates this to the hosts without one.
	#[test]
	fn aac_without_a_platform_encoder_is_refused() {
		let err = open(&aac(Kind::Auto)).err().expect("no AAC encoder on this host");
		let message = err.to_string();
		assert!(message.contains("aac") && message.contains("none"), "{message}");
	}

	/// The stub stands in for the platform tier only, so `Software` still refuses
	/// AAC with it installed.
	#[test]
	fn stub_is_not_software() {
		let _stub = stub::install();
		assert_eq!(open(&aac(Kind::Auto)).unwrap().name(), stub::NAME);

		let err = open(&aac(Kind::Software)).err().expect("no software AAC encoder");
		assert!(err.to_string().contains("no software aac"), "{err}");
	}

	fn named(codec: Codec, name: &str) -> Settings {
		Settings {
			codec,
			kind: Kind::Named(name.into()),
			..Settings::default()
		}
	}

	/// A codec's own name opens that codec, like `Auto`.
	#[test]
	fn codec_names_open_their_codec() {
		let _stub = stub::install();
		assert_eq!(open(&named(Codec::Opus, "opus")).unwrap().name(), libopus::NAME);
		assert_eq!(open(&named(Codec::Pcm, "pcm")).unwrap().name(), pcm::NAME);
		assert_eq!(open(&named(Codec::Aac, "aac")).unwrap().name(), stub::NAME);
	}

	/// A name for another codec is refused, not quietly swapped for the settings' codec.
	#[test]
	fn another_codecs_name_is_refused() {
		assert!(matches!(open(&named(Codec::Opus, "pcm")), Err(Error::Unsupported(_))));
	}

	/// Backend names are internal: asking for one is refused like any unknown name.
	#[test]
	fn backend_names_are_refused() {
		let message = open(&named(Codec::Opus, libopus::NAME))
			.err()
			.expect("libopus is a backend, not a codec")
			.to_string();
		assert!(message.contains("\"libopus\"") && message.contains("opus"), "{message}");
	}
}
