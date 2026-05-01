use std::collections::{BTreeMap, btree_map};

use serde::{Deserialize, Serialize};

use crate::catalog::Container;

/// Information about thumbnail tracks in the catalog.
///
/// Thumbnails are stand-alone still images (e.g. JPEG/PNG/WebP) published at
/// most once every `interval` milliseconds. They let viewers populate a paused
/// player without subscribing to the full video track for an i-frame.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Thumbnail {
	/// A map of track name to rendition configuration.
	/// We use a BTreeMap so keys are sorted alphabetically for *some* deterministic behavior.
	pub renditions: BTreeMap<String, ThumbnailConfig>,
}

impl Thumbnail {
	pub fn is_empty(&self) -> bool {
		self.renditions.is_empty()
	}

	/// Insert a thumbnail config, returning an error if the name already exists.
	pub fn insert(&mut self, name: &str, config: ThumbnailConfig) -> crate::Result<()> {
		let btree_map::Entry::Vacant(entry) = self.renditions.entry(name.to_string()) else {
			return Err(crate::Error::Duplicate(name.to_string()));
		};
		entry.insert(config);
		Ok(())
	}

	/// Remove a track from the catalog by name.
	pub fn remove(&mut self, name: &str) -> Option<ThumbnailConfig> {
		self.renditions.remove(name)
	}
}

/// Thumbnail rendition configuration: a single still-image track.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ThumbnailConfig {
	/// MIME type of the encoded image, e.g. "image/jpeg", "image/png", "image/webp".
	pub codec: String,

	/// Container format for frame encoding.
	/// Defaults to "legacy" for backward compatibility.
	#[serde(default)]
	pub container: Container,

	/// The dimensions of the encoded image in pixels.
	pub coded_width: u32,
	pub coded_height: u32,

	/// The minimum interval between thumbnails in milliseconds.
	/// Subscribers can use this as a hint for how often to poll.
	#[serde(default)]
	pub interval: Option<u64>,

	/// JPEG/WebP quality the publisher targeted (0-1). Informational.
	#[serde(default)]
	pub quality: Option<f64>,
}
