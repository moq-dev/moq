//! Pluggable audio decoder backends.
//!
//! The audio mirror of `moq-video`'s decode backends. [`Backend`] is the seam
//! between the codec and the [`Decoder`](super::Decoder) front end, which owns
//! what every codec shares: trimming the startup delay a backend reports.
//!
//! [`open`] tries the platform decoders before the software ones, skipping any
//! that does not advertise the catalog codec, and refuses when none opens the
//! track. The software tier is libopus for Opus, a passthrough for PCM, and
//! symphonia for AAC-LC mono and stereo (behind the `aac` feature). No platform
//! decoder is wired in yet.

use hang::catalog::{AudioCodec, AudioConfig};

use super::Decoded;
use super::decoder::{Config, Kind};
use crate::{Error, Layout};

mod libopus;
mod pcm;
#[cfg(feature = "aac")]
mod symphonia;

/// An opened decoder: packets in, interleaved `f32` PCM out.
pub(crate) trait Backend: Send {
	/// Decode one packet into interleaved samples at [`sample_rate`](Self::sample_rate)
	/// and [`layout`](Self::layout), untrimmed.
	fn decode(&mut self, packet: &[u8]) -> Result<Decoded, Error>;

	/// Drop codec history after a discontinuity, so the next packet does not
	/// predict from audio that is no longer adjacent.
	fn reset(&mut self) -> Result<(), Error>;

	/// The rate this backend decodes to, which may differ from the catalog's.
	fn sample_rate(&self) -> u32;

	/// The layout this backend decodes to, which may differ from the catalog's.
	fn layout(&self) -> Layout;

	/// Frames at the start of the stream that are codec priming, not media.
	fn delay(&self) -> usize {
		0
	}

	/// The stable lowercase name [`Kind::Named`] selects this backend by.
	fn name(&self) -> &str;
}

type Open = fn(&AudioConfig) -> Result<Box<dyn Backend>, Error>;

/// A backend constructor: its name, the catalog codecs it advertises, and an opener.
struct Candidate {
	name: &'static str,
	supports: fn(&AudioCodec) -> bool,
	open: Open,
}

/// Operating-system decoders, in priority order.
const PLATFORM: &[Candidate] = &[];

const SOFTWARE: &[Candidate] = &[
	Candidate {
		name: libopus::NAME,
		supports: |codec| matches!(codec, AudioCodec::Opus),
		open: libopus::Libopus::open,
	},
	Candidate {
		name: pcm::NAME,
		supports: |codec| matches!(codec, AudioCodec::Pcm),
		open: pcm::Pcm::open,
	},
	// Claims every AAC profile and refuses at open what it can't decode: the
	// profile that matters is the description's, which the catalog string can
	// contradict.
	#[cfg(feature = "aac")]
	Candidate {
		name: symphonia::NAME,
		supports: |codec| matches!(codec, AudioCodec::AAC(_)),
		open: symphonia::Symphonia::open,
	},
];

/// Open the first backend that advertises the catalog codec and accepts the track.
pub(crate) fn open(catalog: &AudioConfig, config: &Config) -> Result<Box<dyn Backend>, Error> {
	select(catalog, &config.kind, candidates(&config.kind, PLATFORM, SOFTWARE))
}

/// The candidates `kind` allows, in the order to try them.
///
/// Takes the tiers as arguments so a test can supply stubs instead of whatever
/// this host compiles in.
fn candidates<'a>(kind: &Kind, platform: &'a [Candidate], software: &'a [Candidate]) -> Vec<&'a Candidate> {
	match kind {
		Kind::Auto => platform.iter().chain(software).collect(),
		Kind::Software => software.iter().collect(),
		Kind::Named(name) => platform.iter().chain(software).filter(|c| c.name == name).collect(),
	}
}

fn select(catalog: &AudioConfig, kind: &Kind, candidates: Vec<&Candidate>) -> Result<Box<dyn Backend>, Error> {
	let codec = &catalog.codec;
	let mut refused = Vec::new();

	for candidate in candidates {
		if !(candidate.supports)(codec) {
			continue;
		}
		match (candidate.open)(catalog) {
			Ok(backend) => return Ok(backend),
			Err(err) => refused.push((candidate.name, err)),
		}
	}

	// One refusal is the whole answer, so keep its variant: a malformed
	// description stays a container error rather than becoming a string.
	if refused.len() == 1 {
		let (_, err) = refused.remove(0);
		return Err(err);
	}
	if !refused.is_empty() {
		let reasons: Vec<String> = refused.iter().map(|(name, err)| format!("{name}: {err}")).collect();
		return Err(Error::Unsupported(reasons.join(", ")));
	}

	match kind {
		Kind::Named(name) => {
			let available: Vec<&str> = PLATFORM
				.iter()
				.chain(SOFTWARE)
				.filter(|c| (c.supports)(codec))
				.map(|c| c.name)
				.collect();
			Err(Error::Unsupported(format!(
				"no audio decoder named {name:?} for {codec} (this build has: {})",
				available.join(", ")
			)))
		}
		_ => Err(Error::Unsupported(format!("unsupported audio codec: {codec}"))),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Activity;

	/// Opens anything it advertises and reports which candidate it came from.
	struct Stub(&'static str);

	impl Backend for Stub {
		fn decode(&mut self, _packet: &[u8]) -> Result<Decoded, Error> {
			Ok(Decoded {
				samples: Vec::new(),
				activity: Activity::Active,
			})
		}

		fn reset(&mut self) -> Result<(), Error> {
			Ok(())
		}

		fn sample_rate(&self) -> u32 {
			48_000
		}

		fn layout(&self) -> Layout {
			Layout::Stereo
		}

		fn name(&self) -> &str {
			self.0
		}
	}

	const PLATFORM_STUB: Candidate = Candidate {
		name: "platform",
		supports: |codec| matches!(codec, AudioCodec::Opus),
		open: |_| Ok(Box::new(Stub("platform"))),
	};

	/// Compiled in but refusing the track, like a platform decoder asked for a
	/// layout its framework does not open.
	const REFUSING: Candidate = Candidate {
		name: "refusing",
		supports: |codec| matches!(codec, AudioCodec::Opus),
		open: |_| Err(Error::Unsupported("not this track".into())),
	};

	const SOFTWARE_STUB: Candidate = Candidate {
		name: "software",
		supports: |codec| matches!(codec, AudioCodec::Opus),
		open: |_| Ok(Box::new(Stub("software"))),
	};

	/// Advertises nothing but PCM, so an Opus track never reaches its opener.
	const PCM_ONLY: Candidate = Candidate {
		name: "pcm-only",
		supports: |codec| matches!(codec, AudioCodec::Pcm),
		open: |_| panic!("opened for a codec it does not advertise"),
	};

	fn opus() -> AudioConfig {
		AudioConfig::new(AudioCodec::Opus, 48_000, 2)
	}

	fn pick(kind: Kind, platform: &[Candidate], software: &[Candidate]) -> Result<String, Error> {
		let backend = select(&opus(), &kind, candidates(&kind, platform, software))?;
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

	/// A named backend that refuses the track is the answer: nothing else is tried.
	#[test]
	fn named_refusal_does_not_fall_back() {
		let err = pick(Kind::Named("refusing".into()), &[REFUSING], &[SOFTWARE_STUB]).unwrap_err();
		assert!(err.to_string().contains("not this track"), "{err}");
	}

	#[test]
	fn every_refusal_is_reported() {
		const ALSO_REFUSING: Candidate = Candidate {
			name: "also-refusing",
			..REFUSING
		};

		let err = pick(Kind::Auto, &[REFUSING], &[ALSO_REFUSING]).unwrap_err();
		let message = err.to_string();
		assert!(message.contains("refusing: ") && message.contains("also-refusing: "), "{message}");
	}

	/// An unknown name says what this build has for the codec instead.
	#[test]
	fn unknown_name_lists_the_alternatives() {
		let err = open(&opus(), &Config {
			kind: Kind::Named("opus".into()),
		})
		.err()
		.expect("no backend is named after its codec");
		let message = err.to_string();
		assert!(message.contains("\"opus\"") && message.contains(libopus::NAME), "{message}");
	}

	/// Asking for a real backend that does not decode the codec is refused, not
	/// quietly swapped for one that does.
	#[test]
	fn named_backend_for_another_codec_is_refused() {
		let config = Config {
			kind: Kind::Named(pcm::NAME.into()),
		};
		assert!(matches!(open(&opus(), &config), Err(Error::Unsupported(_))));
	}

	#[test]
	fn software_backends_open_by_name() {
		let pcm = AudioConfig::new(AudioCodec::Pcm, 48_000, 2);
		let config = Config {
			kind: Kind::Named(pcm::NAME.into()),
		};
		assert_eq!(open(&pcm, &config).unwrap().name(), pcm::NAME);

		let config = Config {
			kind: Kind::Named(libopus::NAME.into()),
		};
		assert_eq!(open(&opus(), &config).unwrap().name(), libopus::NAME);
	}
}
