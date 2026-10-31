use serde::{Deserialize, Serialize};

/// A whole broadcast whose own catalog describes its renditions, listed without opening it.
///
/// Listing one subscribes to nothing: a consumer opens the contribution's catalog only when it
/// selects one of its renditions. Its rendition IDs belong to that broadcast, never the listing
/// catalog.
///
/// Marked `#[non_exhaustive]` so optional fields can be added later. External callers build one
/// with [`Contribution::new`].
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Contribution {
	/// The contribution's broadcast, relative to the broadcast serving this catalog.
	///
	/// Resolve it with [`Path::try_resolve`](moq_net::Path::try_resolve): a reference that walks
	/// above the root names no broadcast, so the catalog is rejected.
	pub broadcast: moq_net::path::Relative,

	/// The publisher instance the reference names, with a rendition `epoch`'s meaning: a route
	/// proving another instance refuses it, and on an epochless route it is best effort. A
	/// composer leaves it out until the contribution resolves.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub epoch: Option<moq_net::Epoch>,
}

impl Contribution {
	/// A contribution at `broadcast`, accepting whichever instance resolves.
	pub fn new(broadcast: impl Into<moq_net::path::Relative>) -> Self {
		Self {
			broadcast: broadcast.into(),
			epoch: None,
		}
	}
}
