//! Transcoder configuration: the rung ladder and catalog wiring.

use moq_net::path::RelativeOwned;

use crate::Ladder;

/// Transcoder configuration for [`run`](crate::run).
///
/// `#[non_exhaustive]`: build via `Config::default()` and set fields, so future
/// knobs don't break callers.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Candidate output renditions, lowest first. Only rungs strictly below the
	/// source survive: a rung is dropped when its height exceeds the source, when
	/// its bitrate is not below the source bitrate (when known), or when it
	/// matches the source height without a known source bitrate to undercut. A
	/// 480p source is never transcoded up to 720p.
	///
	/// Filtering drops rungs but never reorders them, so the surviving ladder is
	/// still ascending. Build it with [`Ladder::new`](crate::Ladder::new), which
	/// takes the rungs in any order and refuses an ambiguous ladder.
	pub ladder: Ladder,

	/// Where the source broadcast lives relative to the output broadcast, e.g.
	/// `"."` when the output is published at `<source>/transcode.hang`. When
	/// set, the derivative catalog references the source renditions (all video
	/// and audio) through this path so players fetch them from the source
	/// directly; the transcoder never proxies or subscribes them. `None` omits
	/// them from the derivative catalog.
	pub source: Option<RelativeOwned>,

	/// Which video encoder implementation encodes the rungs. The default
	/// prefers hardware (NVENC on Linux, VideoToolbox on macOS, Media
	/// Foundation on Windows) and falls back to OpenH264 when enabled.
	pub encoder: moq_video::encode::Kind,

	/// Which video decoder implementation decodes the source. The default
	/// prefers hardware and falls back to OpenH264 when enabled (H.264 only;
	/// H.265 sources need a hardware decoder).
	pub decoder: moq_video::decode::Kind,

	/// Where decoded and resized frames live. Native output keeps GPU-backed
	/// frames on the GPU from decode through encode; CPU output downloads at
	/// the decoder and scales on the CPU.
	pub resize: moq_video::resize::Config,
}

impl Config {
	/// The decoder the shared live feed opens: the configured implementation,
	/// delivering frames where the resize expects them. No scale hint, since
	/// the feed decodes once at native size for every rung.
	pub(crate) fn feed_decoder(&self) -> moq_video::decode::Config {
		let mut decoder = moq_video::decode::Config::new();
		decoder.kind = self.decoder.clone();
		decoder.output = self.resize.output;
		decoder
	}
}
