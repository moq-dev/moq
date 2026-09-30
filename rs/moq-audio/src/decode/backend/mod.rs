//! Pluggable audio decoder backends.
//!
//! The audio mirror of `moq-video`'s decode backends. [`Backend`] is the seam
//! between the codec and the [`Decoder`](super::Decoder) front end, which owns
//! what every codec shares: trimming the startup delay a backend reports.
//!
//! [`open`] tries the platform decoders before the software ones, skipping any
//! that does not advertise the catalog codec, and refuses when none opens the
//! track. [`Kind::Named`] names the codec, not a backend, so which backend
//! decodes it stays the library's choice. The software tier is libopus for Opus, a passthrough for PCM, and
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
///
/// Unwind safe because the published `Decoder` that boxes it is.
pub(crate) trait Backend: Send + std::panic::UnwindSafe + std::panic::RefUnwindSafe {
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

	/// The stable lowercase name of this backend, as [`Decoder::name`](super::Decoder::name)
	/// reports it. Callers never select by it.
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

/// The name [`Kind::Named`] uses for a catalog codec, `None` for one this crate has no name for.
fn codec_name(codec: &AudioCodec) -> Option<&'static str> {
	match codec {
		AudioCodec::Opus => Some("opus"),
		AudioCodec::Pcm => Some("pcm"),
		AudioCodec::AAC(_) => Some("aac"),
		_ => None,
	}
}

/// Open the first backend that advertises the catalog codec and accepts the track.
pub(crate) fn open(catalog: &AudioConfig, config: &Config) -> Result<Box<dyn Backend>, Error> {
	if let Kind::Named(requested) = &config.kind
		&& codec_name(&catalog.codec) != Some(requested)
	{
		return Err(Error::Unsupported(format!(
			"audio decoder {requested:?} is unavailable for {}",
			catalog.codec
		)));
	}

	select(catalog, candidates(&config.kind, PLATFORM, SOFTWARE))
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

fn select(catalog: &AudioConfig, candidates: Vec<&Candidate>) -> Result<Box<dyn Backend>, Error> {
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

	Err(Error::Unsupported(format!("unsupported audio codec: {codec}")))
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
		let backend = select(&opus(), candidates(&kind, platform, software))?;
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
		let name = pick(Kind::Named("opus".into()), &[PLATFORM_STUB], &[SOFTWARE_STUB]).unwrap();
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

	fn named(name: &str) -> Config {
		Config {
			kind: Kind::Named(name.into()),
		}
	}

	/// The codec names published moq-audio accepts still open their codec.
	#[test]
	fn codec_names_open_their_codec() {
		let pcm = AudioConfig::new(AudioCodec::Pcm, 48_000, 2);
		assert_eq!(open(&pcm, &named("pcm")).unwrap().name(), pcm::NAME);
		assert_eq!(open(&opus(), &named("opus")).unwrap().name(), libopus::NAME);
	}

	/// A name for another codec is refused, not quietly swapped for one that decodes.
	#[test]
	fn another_codecs_name_is_refused() {
		assert!(matches!(open(&opus(), &named("pcm")), Err(Error::Unsupported(_))));
	}

	/// Backend names are internal: asking for one is refused like any unknown name.
	#[test]
	fn backend_names_are_refused() {
		let message = open(&opus(), &named(libopus::NAME))
			.err()
			.expect("libopus is a backend, not a codec")
			.to_string();
		assert!(message.contains("\"libopus\"") && message.contains("opus"), "{message}");
	}
}
