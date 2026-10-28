use serde::{Deserialize, Serialize};

/// The publisher's media snapshot track in this broadcast.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Stats {
	/// The name of the publisher snapshot track.
	pub track: String,
}

impl Stats {
	/// Name the publisher snapshot track.
	pub fn new(track: impl Into<String>) -> Self {
		Self { track: track.into() }
	}
}
