use std::collections::{BTreeMap, btree_map};

use serde::{Deserialize, Serialize};

use crate::catalog::{Compression, Mode, Timeline};

/// The binary tracks a broadcast publishes, keyed by track name.
///
/// Each frame is an opaque blob: a thumbnail, a serialized state snapshot, a sequence of samples in
/// some application format. Each entry says how to read the track ([`mode`](BinaryConfig::mode) and
/// [`compression`](BinaryConfig::compression)) without a consumer having to know the application.
/// Use [`Json`](crate::catalog::Json) instead when the payloads are JSON, so a generic consumer can
/// read them.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without bumping the major
/// version. External callers start from [`Binary::default`] and fill in what they need
/// ([`insert`](Self::insert)); struct-literal construction is not available outside this crate.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Binary {
	/// A map of track name to configuration.
	///
	/// A map rather than an array so it works with JSON Merge Patch, and a `BTreeMap` so keys are
	/// sorted for *some* deterministic behavior.
	pub tracks: BTreeMap<String, BinaryConfig>,
}

impl crate::catalog::Section for Binary {
	const MAP: &'static str = "tracks";
}

impl Binary {
	/// Insert a track config, returning an error if the name already exists.
	pub fn insert(&mut self, name: &str, config: BinaryConfig) -> crate::Result<()> {
		let btree_map::Entry::Vacant(entry) = self.tracks.entry(name.to_string()) else {
			return Err(crate::Error::Duplicate(name.to_string()));
		};
		entry.insert(config);
		Ok(())
	}

	/// Remove the track from the catalog and return its configuration if found.
	pub fn remove(&mut self, name: &str) -> Option<BinaryConfig> {
		self.tracks.remove(name)
	}

	/// Whether there are no binary tracks, in which case the section is left off the wire.
	pub fn is_empty(&self) -> bool {
		self.tracks.is_empty()
	}
}

/// How to read one binary track.
///
/// Each frame is an opaque payload; [`mode`](Self::mode) says how the track's groups compose those
/// frames into what a consumer sees.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without bumping the major
/// version. External callers build one with [`BinaryConfig::new`] and assign the optional fields
/// they need; struct-literal construction is not available outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct BinaryConfig {
	/// Optional reference to another broadcast that publishes this track, expressed relative to the
	/// broadcast that served this catalog (e.g. `./source`). If unset, the track lives in the same
	/// broadcast as the catalog.
	#[serde(default)]
	pub broadcast: Option<moq_net::PathRelativeOwned>,

	/// Whether the track is a latest-value blob or an append log. Always stated: see [`Mode`].
	#[serde_as(as = "serde_with::DisplayFromStr")]
	pub mode: Mode,

	/// The compression applied to each frame, or `None` when they are written through untouched.
	#[serde(default)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub compression: Option<Compression>,

	/// An optional media type for each payload (e.g. `image/jpeg`). Purely descriptive: a consumer
	/// that doesn't recognize it can still read the track.
	#[serde(default)]
	pub mime: Option<String>,

	/// The companion timeline track indexing this track's groups, if the publisher offers one.
	#[serde(default)]
	pub timeline: Option<Timeline>,

	/// Fields this build doesn't recognize, kept so the entry round-trips.
	///
	/// A future [`Mode`] or [`Compression`] almost certainly comes with fields describing it, and
	/// preserving the discriminant without them would leave a relay republishing an entry nothing
	/// can act on. Same reason [`Container`](crate::catalog::Container) keeps an unknown container
	/// verbatim.
	#[serde(flatten)]
	pub extra: serde_json::Map<String, serde_json::Value>,
}

impl BinaryConfig {
	/// A config for a track read in `mode`, uncompressed and with no optional fields set.
	pub fn new(mode: Mode) -> Self {
		Self {
			broadcast: None,
			mode,
			compression: None,
			mime: None,
			timeline: None,
			extra: Default::default(),
		}
	}
}
