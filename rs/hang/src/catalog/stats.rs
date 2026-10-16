use serde::{Deserialize, Serialize};

/// The publisher's media snapshot track in this broadcast.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
#[non_exhaustive]
pub struct Stats {
	/// The name of the publisher snapshot track.
	pub track: String,
}
