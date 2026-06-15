use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A custom, application-defined data track in the catalog.
///
/// Unlike `audio`/`video`, the catalog says nothing about how to decode a data track. It just
/// advertises that the track exists (keyed by name in the [`Data`] map) so a consumer can discover
/// and subscribe to it. The optional fields are hints for the consumer, not instructions for hang.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct DataTrack {
	/// The MIME type of each frame's payload, e.g. `"application/json"`. Informational.
	pub mime: Option<String>,

	/// A free-form description of the track's contents, for humans.
	pub description: Option<String>,
}

/// The `data` catalog section: a map of track name to [`DataTrack`].
///
/// Each key is the name of a track within the broadcast (e.g. `"meta.json"`). The tracks are
/// independent of each other, so this is a flat map rather than the rendition groups used by
/// `audio`/`video`.
pub type Data = BTreeMap<String, DataTrack>;
