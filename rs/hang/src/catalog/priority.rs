//! Default delivery priorities for hang tracks.

/// Delivery priorities for each hang track kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct Priorities {
	/// Catalog track priority.
	pub catalog: u8,
	/// Audio track priority.
	pub audio: u8,
	/// Video track priority.
	pub video: u8,
}

/// Default delivery priorities for hang tracks, with higher values sent first.
pub const PRIORITY: Priorities = Priorities {
	catalog: 100,
	audio: 80,
	video: 60,
};

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn media_priorities_match_the_browser() {
		assert_eq!(PRIORITY.catalog, 100);
		assert_eq!(PRIORITY.audio, 80);
		assert_eq!(PRIORITY.video, 60);
	}
}
