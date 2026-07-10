//! Transcoder configuration: the rung ladder and catalog wiring.

use moq_net::PathRelativeOwned;

/// One candidate output rendition: a target resolution (by height) and bitrate.
///
/// The width is derived from the source aspect ratio at runtime, and a rung is
/// only offered when it is strictly below the source (see [`Config::rungs`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Rung {
	/// Output height in pixels. Rounded down to even (I420 chroma is 2x2).
	pub height: u32,

	/// Target bitrate in bits per second: the CBR target and the bitrate
	/// advertised in the derivative catalog.
	pub bitrate: u64,
}

impl Rung {
	/// A rung at `height` pixels and `bitrate` bits per second.
	pub fn new(height: u32, bitrate: u64) -> Self {
		Self { height, bitrate }
	}
}

/// Transcoder configuration for [`run`](crate::run).
///
/// `#[non_exhaustive]`: build via `Config::default()` and set fields, so future
/// knobs don't break callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Candidate output renditions. Only rungs strictly below the source
	/// survive: a rung is dropped when its height exceeds the source, when its
	/// bitrate is not below the source bitrate (when known), or when it matches
	/// the source height without a known source bitrate to undercut. A 480p
	/// source is never transcoded up to 720p.
	pub rungs: Vec<Rung>,

	/// Where the source broadcast lives relative to the output broadcast, e.g.
	/// `".."` when the output is published at `<source>/transcode.hang`. When
	/// set, the derivative catalog references the source renditions (all video
	/// and audio) through this path so players fetch them from the source
	/// directly; the transcoder never proxies or subscribes them. `None` omits
	/// them from the derivative catalog.
	pub source: Option<PathRelativeOwned>,

	/// Which video encoder implementation encodes the rungs. The default
	/// prefers hardware (NVENC on Linux, VideoToolbox on macOS, Media
	/// Foundation on Windows) and falls back to openh264.
	pub encoder: moq_video::encode::Kind,

	/// Which video decoder implementation decodes the source. The default
	/// prefers hardware and falls back to openh264 (H.264 only; H.265 sources
	/// need a hardware decoder).
	pub decoder: moq_video::decode::Kind,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			// The default ladder, top rung first, filtered against the source at
			// runtime so only strictly-lower renditions are offered.
			rungs: vec![
				Rung::new(1080, 5_000_000),
				Rung::new(720, 2_500_000),
				Rung::new(480, 1_200_000),
				Rung::new(360, 600_000),
				Rung::new(240, 350_000),
			],
			source: None,
			encoder: moq_video::encode::Kind::default(),
			decoder: moq_video::decode::Kind::default(),
		}
	}
}
