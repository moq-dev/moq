//! The `play` verb's command-line surface.

use moq_mux::catalog::CatalogFormat;

use crate::subscribe::{CatalogFormatArg, SelectArgs};

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

	/// The released spelling of [`Self::delay`], kept in the parser only so a
	/// process that still passes it is told what to pass instead.
	#[usage(long, alias = "latency-max", hide = true)]
	pub max_age: Option<moq_tokio::Duration>,

	/// Rendition selection by track name or codec.
	#[usage(flatten)]
	pub select: SelectArgs,
}

impl Args {
	/// Every released spelling this stage was parsed from.
	///
	/// `--delay` is not a rename: it holds the picture back as well as bounding
	/// staleness, so a command line that still passes `--max-age` is refused with
	/// the difference spelled out rather than quietly given a playout offset.
	pub fn deprecated(&self) -> moq_tokio::Deprecated {
		let mut found = moq_tokio::Deprecated::default();
		if self.max_age.is_some() {
			found.changed(
				"--max-age",
				None,
				"--delay",
				"it is the playout delay as well as the staleness budget, so playback trails the live edge by it",
			);
		}
		found
	}

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
		let max = moq_audio::playback::Input::LATENCY_MAX;
		anyhow::ensure!(
			self.delay.into_std() <= max,
			"--delay must be at most {max:?}; it is the depth the speaker buffers"
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
	/// `--max-age` no longer takes effect on `play`, but it still parses into the
	/// hidden field, which is what lets the refusal name its replacement instead
	/// of leaving an operator with "unexpected argument".
	#[test]
	fn the_delay_replaces_the_staleness_budget() {
		assert_eq!(parse(&[]).delay.into_std(), std::time::Duration::from_millis(100));
		assert_eq!(
			parse(&["--delay", "500ms"]).delay.into_std(),
			std::time::Duration::from_millis(500)
		);
		assert!(parse(&[]).deprecated().is_empty());

		for released in [["--max-age", "500ms"], ["--latency-max", "500ms"]] {
			let refusal = parse(&released).deprecated().to_string();
			assert!(refusal.contains("--max-age"), "{refusal}");
			assert!(refusal.contains("--delay"), "{refusal}");
		}
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
