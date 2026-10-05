//! The `play` verb's command-line surface.

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use moq_mux::catalog::CatalogFormat;

use crate::subscribe::{CatalogFormatArg, SelectArgs};

/// The longest delay the speaker can hold, which is what bounds `--delay`.
///
/// Duplicated from `moq_audio::playback::Input::LATENCY_MAX` because this module
/// compiles without the `play` feature, and so without that crate. The test
/// below pins the two together in a build that has both.
const DELAY_MAX: Duration = Duration::from_secs(10);

/// How long `auto` waits on a stalled group before skipping it.
///
/// The estimate only sees what the container hands over, and a skipped group is
/// never handed over, so a budget under the estimate's ceiling would cap the
/// target at the budget it was cut to. Two seconds is that ceiling, the most
/// lateness the estimate can represent.
const AUTO_MAX_AGE: Duration = Duration::from_secs(2);

/// How far video alone trails the live edge under `auto`. The estimate is
/// audio's; video follows the speaker whenever there is one.
const AUTO_VIDEO_DELAY: Duration = Duration::from_millis(100);

/// How far playback trails the live edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Delay {
	/// Sized from how unevenly audio arrives, and following it as that changes.
	Auto,
	/// Held at exactly this.
	Fixed(Duration),
}

impl FromStr for Delay {
	type Err = humantime::DurationError;

	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value {
			"auto" => Ok(Self::Auto),
			value => humantime::parse_duration(value).map(Self::Fixed),
		}
	}
}

impl fmt::Display for Delay {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Auto => f.write_str("auto"),
			Self::Fixed(delay) => humantime::format_duration(*delay).fmt(f),
		}
	}
}

/// Play one MoQ broadcast through a native window and speaker.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// Catalog format, detected from the broadcast suffix when omitted.
	#[usage(long, value_enum)]
	pub catalog_format: Option<CatalogFormatArg>,

	/// How far playback trails the live edge: `auto`, or a fixed duration.
	///
	/// The playout delay: every frame is presented this long after the live edge, which is
	/// how late one may arrive and still make its slot. `auto` measures how unevenly audio
	/// arrives and follows it, so a publisher that flushes in bursts gets a buffer deep
	/// enough to play through them. A duration fixes the delay instead, and doubles as the
	/// staleness budget, since nothing older than the playhead is worth presenting. The
	/// speaker holds the delay, with a 50ms floor under it, so a smaller value than that
	/// does not reach the picture either.
	#[usage(long, default = "auto")]
	pub delay: Delay,

	/// Rendition selection by track name or codec.
	#[usage(flatten)]
	pub select: SelectArgs,
}

impl Args {
	pub(super) fn catalog_format(&self, broadcast: &str) -> CatalogFormat {
		self.catalog_format
			.map(Into::into)
			.or_else(|| CatalogFormat::detect(broadcast))
			.unwrap_or_default()
	}

	/// The fixed playout delay, or `None` to estimate it.
	pub(super) fn fixed_delay(&self) -> Option<Duration> {
		match self.delay {
			Delay::Auto => None,
			Delay::Fixed(delay) => Some(delay),
		}
	}

	/// How long a stalled group is waited on before it is skipped.
	pub(super) fn max_age(&self) -> Duration {
		self.fixed_delay().unwrap_or(AUTO_MAX_AGE)
	}

	/// How far video trails the live edge while no speaker is setting the pace.
	pub(super) fn video_delay(&self) -> Duration {
		self.fixed_delay().unwrap_or(AUTO_VIDEO_DELAY)
	}

	/// Reject a codec the local decoders can't open.
	///
	/// The selection flags are shared with the stdout exports, which pass bytes
	/// through and so accept every codec the catalog can name. Asking for one of
	/// those here would filter the catalog down to a rendition that then fails to
	/// decode, leaving a blank window rather than an error.
	pub fn validate(&self) -> anyhow::Result<()> {
		use crate::subscribe::VideoCodecArg;

		anyhow::ensure!(
			!matches!(self.select.video_codec, Some(VideoCodecArg::Vp8 | VideoCodecArg::Vp9)),
			"`play` cannot decode vp8 or vp9; pass --video-codec h264, h265, or av1"
		);
		// The delay is the speaker's ring depth, so a value it cannot hold is
		// refused here rather than after the pipeline has opened a device.
		anyhow::ensure!(
			self.fixed_delay().is_none_or(|delay| delay <= DELAY_MAX),
			"--delay must be at most {DELAY_MAX:?}; it is the depth the speaker buffers"
		);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	#[derive(usage::Cli)]
	struct Cli {
		#[usage(flatten)]
		args: Args,
	}

	fn parse(flags: &[&str]) -> Args {
		let argv: Vec<&std::ffi::OsStr> = flags.iter().map(std::ffi::OsStr::new).collect();
		Cli::parse_from(&argv).expect("parse the play flags").args
	}

	/// The selection flags are shared with the stdout exports, which pass every
	/// codec through, so a codec the local decoders can't open is only caught
	/// here. Missing it leaves a blank window rather than an error.
	#[test]
	fn undecodable_video_codecs_are_refused() {
		parse(&[]).validate().unwrap();
		parse(&["--video-codec", "h264"]).validate().unwrap();
		parse(&["--video-codec", "av1"]).validate().unwrap();

		let err = parse(&["--video-codec", "vp9"]).validate().unwrap_err().to_string();
		assert!(err.contains("vp8 or vp9"), "{err}");
		assert!(parse(&["--video-codec", "vp8"]).validate().is_err());
	}

	/// A fixed delay is the playout offset and the staleness budget at once.
	#[test]
	fn a_fixed_delay_sets_the_staleness_budget() {
		let args = parse(&["--delay", "500ms"]);
		assert_eq!(args.fixed_delay(), Some(Duration::from_millis(500)));
		assert_eq!(args.max_age(), Duration::from_millis(500));
		assert_eq!(args.video_delay(), Duration::from_millis(500));
	}

	/// The default measures the delay, and waits on a stalled group as long as the
	/// estimate can see, so the budget never caps what it measures.
	#[test]
	fn the_delay_defaults_to_the_estimate() {
		let args = parse(&[]);
		assert_eq!(args.delay, Delay::Auto);
		assert_eq!(args.fixed_delay(), None);
		assert_eq!(args.max_age(), AUTO_MAX_AGE);
		assert_eq!(parse(&["--delay", "auto"]).delay, Delay::Auto);
		assert_eq!(Delay::Auto.to_string(), "auto");
		assert_eq!(Delay::Fixed(Duration::from_millis(250)).to_string(), "250ms");
	}

	/// The playout budget has one spelling because it always controls both
	/// presentation delay and media staleness.
	#[test]
	fn the_playout_budget_has_one_flag() {
		for removed in [["--max-age", "500ms"], ["--latency-max", "500ms"]] {
			let argv: Vec<&std::ffi::OsStr> = removed.iter().map(std::ffi::OsStr::new).collect();
			assert!(Cli::parse_from(&argv).is_err(), "{} still parsed", removed[0]);
		}
	}

	/// A depth the speaker cannot hold is refused up front, rather than after the
	/// pipeline has opened a device.
	#[test]
	fn the_delay_is_bounded_by_the_speakers_ring() {
		let err = parse(&["--delay", "11s"]).validate().unwrap_err().to_string();
		assert!(err.contains("--delay must be at most"), "{err}");
		parse(&["--delay", "10s"]).validate().unwrap();
		parse(&["--delay", "auto"]).validate().unwrap();

		// The bound has to be the sink's own, which only a build carrying the sink
		// can say.
		#[cfg(feature = "play")]
		assert_eq!(DELAY_MAX, moq_audio::playback::Input::LATENCY_MAX);
	}

	/// The suffix picks the format, and the flag overrides it.
	#[test]
	fn the_catalog_format_follows_the_broadcast_suffix() {
		assert_eq!(parse(&[]).catalog_format("room.hang"), CatalogFormat::Hang);
		assert_eq!(parse(&[]).catalog_format("room.msf"), CatalogFormat::Msf);
		assert_eq!(parse(&[]).catalog_format("room"), CatalogFormat::DEFAULT);
		assert_eq!(
			parse(&["--catalog-format", "hangz"]).catalog_format("room.msf"),
			CatalogFormat::HangZ
		);
	}
}
