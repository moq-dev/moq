//! Configuration for per-frame fMP4 fragmenting.

/// How duration-less video frames are handled.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub enum MissingDuration {
	/// Reject duration-less video because decode duration cannot be derived from reordered PTS.
	#[default]
	Reject,
	/// Infer from the successor PTS, for video that is known to have no frame reordering.
	InferFromPresentationTime,
}

/// Options for creating a per-frame [`Fragmenter`](super::Fragmenter).
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct Config {
	/// Policy for duration-less video frames; audio can always infer from its successor.
	pub missing_duration: MissingDuration,
}
