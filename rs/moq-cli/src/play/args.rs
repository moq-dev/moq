//! The `play` verb's command-line surface.

use std::time::Duration;

use clap::Args as ClapArgs;
use moq_mux::catalog::CatalogFormat;

use crate::subscribe::{CatalogFormatArg, SelectArgs};

/// Play one MoQ broadcast through a native window and speaker.
#[derive(ClapArgs, Clone)]
pub struct Args {
	/// Catalog format, detected from the broadcast suffix when omitted.
	#[arg(long)]
	pub catalog_format: Option<CatalogFormatArg>,

	/// Maximum media buffering before skipping a stalled group.
	#[arg(long, default_value = "500ms", value_parser = humantime::parse_duration)]
	pub latency_max: Duration,

	/// Rendition selection by track name or codec.
	#[command(flatten)]
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
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use clap::Parser;

	/// `Args` is flattened into a subcommand, so give it a root to parse under.
	#[derive(Parser)]
	struct Cli {
		#[command(flatten)]
		args: Args,
	}

	fn parse(flags: &[&str]) -> Args {
		let mut argv = vec!["play"];
		argv.extend_from_slice(flags);
		Cli::try_parse_from(argv).expect("parse the play flags").args
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
