use std::collections::{BTreeMap, btree_map};

use serde::{Deserialize, Serialize};

use crate::catalog::{Compression, Mode, Timeline};

/// The JSON tracks a broadcast publishes, keyed by track name.
///
/// These are application data tracks, not media: a chat log, a telemetry feed, a status document.
/// Each entry says how to read the track ([`mode`](JsonConfig::mode) and
/// [`compression`](JsonConfig::compression)) without a consumer having to know the application.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without bumping the major
/// version. External callers start from [`Json::default`] and fill in what they need
/// ([`insert`](Self::insert)); struct-literal construction is not available outside this crate.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Json {
	/// A map of track name to configuration.
	///
	/// A map rather than an array so it works with JSON Merge Patch, and a `BTreeMap` so keys are
	/// sorted for *some* deterministic behavior.
	pub tracks: BTreeMap<String, JsonConfig>,
}

impl crate::catalog::Section for Json {
	const MAP: &'static str = "tracks";
}

impl Json {
	/// Insert a track config, returning an error if the name already exists.
	pub fn insert(&mut self, name: &str, config: JsonConfig) -> crate::Result<()> {
		let btree_map::Entry::Vacant(entry) = self.tracks.entry(name.to_string()) else {
			return Err(crate::Error::Duplicate(name.to_string()));
		};
		entry.insert(config);
		Ok(())
	}

	/// Remove the track from the catalog and return its configuration if found.
	pub fn remove(&mut self, name: &str) -> Option<JsonConfig> {
		self.tracks.remove(name)
	}

	/// Whether there are no JSON tracks, in which case the section is left off the wire.
	pub fn is_empty(&self) -> bool {
		self.tracks.is_empty()
	}
}

/// How to read one JSON track.
///
/// Each frame is a UTF-8 JSON value; [`mode`](Self::mode) says how the track's groups compose
/// those frames into the value a consumer sees.
///
/// Marked `#[non_exhaustive]` so additional optional fields can be added without bumping the major
/// version. External callers build one with [`JsonConfig::new`] and assign the optional fields they
/// need; struct-literal construction is not available outside this crate.
#[serde_with::serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct JsonConfig {
	/// Optional reference to another broadcast that publishes this track, expressed relative to the
	/// broadcast that served this catalog (e.g. `./source`). If unset, the track lives in the same
	/// broadcast as the catalog.
	#[serde(default)]
	pub broadcast: Option<moq_net::PathRelativeOwned>,

	/// Whether the track is a latest-value document or an append log. Always stated: see [`Mode`].
	#[serde_as(as = "serde_with::DisplayFromStr")]
	pub mode: Mode,

	/// The compression applied to each frame, or `None` when they are plaintext.
	#[serde(default)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub compression: Option<Compression>,

	/// An optional identifier for the shape of each value, typically a JSON Schema URL. Purely
	/// descriptive: a consumer that doesn't recognize it can still read the track.
	#[serde(default)]
	pub schema: Option<String>,

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

impl JsonConfig {
	/// A config for a track read in `mode`, uncompressed and with no optional fields set.
	pub fn new(mode: Mode) -> Self {
		Self {
			broadcast: None,
			mode,
			compression: None,
			schema: None,
			timeline: None,
			extra: Default::default(),
		}
	}
}
