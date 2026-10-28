//! Default delivery priorities for hang tracks.

/// Delivery priorities for each hang track kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Priorities {
	/// Catalog track priority.
	pub catalog: i8,
	/// Text (caption/subtitle) track priority.
	pub text: i8,
	/// Audio track priority.
	pub audio: i8,
	/// Video track priority.
	pub video: i8,
}

/// Default delivery priorities for hang tracks, with higher values sent first.
///
/// All of them sit above 0, the unset default, so hang media outranks a track that never set
/// a priority. Text sits just below the catalog and above audio/video: cues are tiny and accessibility-critical,
/// so they should never be starved behind a media backlog.
pub const PRIORITY: Priorities = Priorities {
	catalog: 100,
	text: 90,
	audio: 80,
	video: 60,
};
