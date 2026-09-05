//! Transcoder configuration: the rung ladder and catalog wiring.

use moq_net::{AsPath, PathRelativeOwned};

use crate::Ladder;

#[doc(hidden)]
#[deprecated(note = "use moq_net::Path::relative")]
pub fn source_reference(source: impl AsPath, output: impl AsPath) -> Option<PathRelativeOwned> {
	let source = source.as_path();
	let output = output.as_path();
	if output.strip_prefix(&source)?.is_empty() {
		return None;
	}

	source.relative(&output)
}

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
	pub rungs: Ladder,

	/// Where the source broadcast lives relative to the output broadcast, e.g.
	/// `"."` when the output is published at `<source>/transcode.hang`. When
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

	/// Frame resize behavior. Automatic mode keeps GPU-backed frames on the GPU.
	pub resize: moq_video::resize::Config,
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
	use super::*;

	#[test]
	fn source_reference_normalizes_and_counts_output_depth() {
		assert_eq!(source_reference("a/b", "a/b/transcode.hang").unwrap().as_str(), ".");
		assert_eq!(source_reference("/a//b/", "a/b/dir/").unwrap().as_str(), ".");
		assert_eq!(
			source_reference("a/b", "a/b/dir/transcode.hang").unwrap().as_str(),
			".."
		);
		assert_eq!(
			source_reference("a/b", "a/b/one/two/transcode.hang").unwrap().as_str(),
			"../.."
		);
		assert!(source_reference("a/b", "other/transcode.hang").is_none());
		assert!(source_reference("a/b", "a/b").is_none());
	}
}
