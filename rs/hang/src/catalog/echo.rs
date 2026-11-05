use serde::{Deserialize, Serialize};

/// The relative broadcast prefix under which viewers may publish feedback.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Echo {
	/// The feedback prefix relative to the broadcast serving this catalog.
	pub path: moq_net::path::Relative,
}

impl Echo {
	/// Solicit feedback under a prefix relative to the catalog's broadcast.
	pub fn new(path: impl Into<moq_net::path::Relative>) -> Self {
		Self { path: path.into() }
	}
}
