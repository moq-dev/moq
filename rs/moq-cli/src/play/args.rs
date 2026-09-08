//! The `play` verb's command-line surface.

use std::time::Duration;

use moq_mux::catalog::CatalogFormat;

use crate::subscribe::{CatalogFormatArg, SelectArgs};

/// The longest delay the speaker can hold, which is what bounds `--delay`.
///
/// Duplicated from `moq_audio::playback::Input::LATENCY_MAX` because this module
/// compiles without the `play` feature, and so without that crate. The test
/// below pins the two together in a build that has both.
const DELAY_MAX: Duration = Duration::from_secs(10);

/// Play one MoQ broadcast through a native window and speaker.
#[derive(usage::Args, Clone)]
#[usage(unknown_flags = "error", args_override_self = false)]
pub struct Args {
	/// Catalog format, detected from the broadcast suffix when omitted.
	#[usage(long, value_enum)]
	pub catalog_format: Option<CatalogFormatArg>,

	/// How far playback trails the live edge.
	///
	/// The playout delay: every frame is presented this long after the live edge, which is
	/// how late one may arrive and still make its slot. It doubles as the staleness budget,
	/// since nothing older than the playhead is worth presenting. The speaker holds the
	/// delay, with a 50ms floor under it, so a smaller value than that does not reach the
	/// picture either.
	#[usage(long, default = "100ms")]
	pub delay: moq_tokio::Duration,

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
			self.delay.into_std() <= DELAY_MAX,
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

	/// The delay is the playout offset and the staleness budget at once, so its
	/// default has to be one a live stream can actually present against.
	#[test]
	fn the_delay_sets_the_staleness_budget() {
		assert_eq!(parse(&[]).delay.into_std(), std::time::Duration::from_millis(100));
		assert_eq!(
			parse(&["--delay", "500ms"]).delay.into_std(),
			std::time::Duration::from_millis(500)
		);
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
