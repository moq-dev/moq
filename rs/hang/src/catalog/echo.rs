use serde::{Deserialize, Serialize};

/// The relative broadcast prefix under which viewers may publish feedback.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[non_exhaustive]
pub struct Echo {
	/// The feedback prefix relative to the broadcast serving this catalog.
	pub path: moq_net::path::RelativeOwned,
}
