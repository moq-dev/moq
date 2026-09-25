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
//! that does not emit the codec, and refuses when none opens. The software tier
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

	/// The stable lowercase name [`Kind::Named`] selects this backend by.
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

/// Test-only backends, in neither tier so `Auto` and `Software` never pick one:
/// they exist to be asked for by name.
#[cfg(test)]
const NAMED_ONLY: &[Candidate] = &[Candidate {
	name: stub::NAME,
	codecs: &[Codec::Aac],
	open: stub::Stub::open,
}];

#[cfg(not(test))]
const NAMED_ONLY: &[Candidate] = &[];

/// Open the first backend that emits the codec and accepts the settings.
pub(crate) fn open(settings: &Settings) -> Result<Box<dyn Backend>, Error> {
	select(settings, candidates(&settings.kind, PLATFORM, SOFTWARE))
}

/// The candidates `kind` allows, in the order to try them.
///
/// Takes the tiers as arguments so a test can supply stubs instead of whatever
/// this host compiles in.
fn candidates<'a>(kind: &Kind, platform: &'a [Candidate], software: &'a [Candidate]) -> Vec<&'a Candidate> {
	match kind {
		Kind::Auto => platform.iter().chain(software).collect(),
		Kind::Software => software.iter().collect(),
		Kind::Named(name) => platform
			.iter()
			.chain(software)
			.chain(NAMED_ONLY)
			.filter(|c| c.name == name)
			.collect(),
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
		Kind::Named(name) => format!("no audio encoder named {name:?} for {codec} (this build has: {available})"),
		Kind::Software => format!("no software {codec} audio encoder (this build has: {available})"),
		Kind::Auto => format!("no {codec} audio encoder (this build has: {available})"),
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

	#[test]
	fn named_forces_one() {
		let name = pick(
			Kind::Named("software".into()),
			&[PLATFORM_STUB],
			&[PCM_ONLY, SOFTWARE_STUB],
		)
		.unwrap();
		assert_eq!(name, "software");
	}

	/// A named backend that refuses the settings is the answer: nothing else is tried.
	#[test]
	fn named_refusal_does_not_fall_back() {
		let err = pick(Kind::Named("refusing".into()), &[REFUSING], &[SOFTWARE_STUB]).unwrap_err();
		assert!(err.to_string().contains("not these settings"), "{err}");
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

	/// An unknown name says what this build has for the codec instead.
	#[test]
	fn unknown_name_lists_the_alternatives() {
		let settings = Settings {
			kind: Kind::Named("opus".into()),
			..Settings::default()
		};
		let message = open(&settings)
			.err()
			.expect("no backend is named after its codec")
			.to_string();
		assert!(
			message.contains("\"opus\"") && message.contains(libopus::NAME),
			"{message}"
		);
	}

	#[test]
	fn software_backends_open_by_name() {
		let settings = Settings {
			kind: Kind::Named(libopus::NAME.into()),
			..Settings::default()
		};
		assert_eq!(open(&settings).unwrap().name(), libopus::NAME);

		let settings = Settings {
			codec: Codec::Pcm,
			kind: Kind::Named(pcm::NAME.into()),
			..Settings::default()
		};
		assert_eq!(open(&settings).unwrap().name(), pcm::NAME);
	}
}
