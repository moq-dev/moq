use serde::{Deserialize, Serialize};

/// A named track reference in the catalog.
///
/// Just the track name; a subscriber learns the track's properties
/// (compression, timescale, cache) from SUBSCRIBE_OK, not the catalog.
/// Matches the JS `TrackSchema` (`{ "name": "..." }`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Track {
	/// The track name within the broadcast.
	pub name: String,
}

impl Track {
	/// Create a track reference with the given name.
	pub fn new(name: impl Into<String>) -> Self {
		Self { name: name.into() }
	}
}

impl<T: Into<String>> From<T> for Track {
	fn from(name: T) -> Self {
		Self::new(name)
	}
}
