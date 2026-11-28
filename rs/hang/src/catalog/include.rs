use serde::{Deserialize, Serialize};

/// A whole broadcast whose own catalog describes its renditions, listed without opening it.
///
/// It shares the listing catalog's PTS timeline and root settings, including `sync.clock` and
/// `sync.interval`; its own catalog's settings are ignored.
///
/// Listing one subscribes to nothing: a consumer opens the include's catalog only when it
/// selects it, including an HLS master listing its renditions. An on-demand publisher answers
/// the catalog without starting media. Its rendition IDs belong to that broadcast, never the
/// listing catalog.
///
/// Marked `#[non_exhaustive]` so optional fields can be added later. External callers build one
/// with [`Include::new`].
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Include {
	/// The include's broadcast, relative to the broadcast serving this catalog.
	///
	/// Resolve it with [`Path::try_resolve`](moq_net::Path::try_resolve): a reference that walks
	/// above the root names no broadcast, so the catalog is rejected.
	pub broadcast: moq_net::path::Relative,
}

impl crate::catalog::Entry for Include {}

impl Include {
	/// An include at `broadcast`, accepting whichever stream resolves.
	pub fn new(broadcast: impl Into<moq_net::path::Relative>) -> Self {
		Self {
			broadcast: broadcast.into(),
		}
	}
}
