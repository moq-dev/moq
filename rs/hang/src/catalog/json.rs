use std::collections::{BTreeMap, btree_map};

use serde::{Deserialize, Serialize};

use crate::catalog::Mode;
use crate::catalog::millis::MillisCeil;

/// The JSON tracks a broadcast publishes, keyed by track name.
///
/// These are application data tracks, not media: a chat log, a telemetry feed, a status document.
/// Each entry says how to read the track (its [`mode`](JsonConfig::mode)) without a consumer having to
/// know the application.
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
	pub broadcast: Option<moq_net::path::Relative>,

	/// The track's name in its broadcast, when it differs from this entry's ID: its key in the
	/// catalog. Absent means the ID is the track name, as in every catalog before this field.
	///
	/// Set it only when the ID can't be the name, such as two renditions of same-named tracks
	/// from different broadcasts. Readers resolve the name with
	/// [`Reference::track`](crate::catalog::Reference::track); one that predates the
	/// field subscribes to the ID instead.
	#[serde(default)]
	pub track: Option<String>,

	/// The publisher instance the reference names, when the catalog knows it. Absent names
	/// whichever instance the reader resolves first, as in every catalog before this field.
	///
	/// A reader on an epoch-aware route refuses another instance rather than fall back to it;
	/// on an epochless route it is best effort, since the route cannot prove the instance.
	/// Without [`broadcast`](Self::broadcast), it names the catalog's own instance.
	#[serde(default)]
	#[serde_as(as = "Option<serde_with::DisplayFromStr>")]
	pub epoch: Option<moq_net::Epoch>,

	/// Whether the track is a latest-value document or an append log. Always stated: see [`Mode`].
	#[serde_as(as = "serde_with::DisplayFromStr")]
	pub mode: Mode,

	/// An optional identifier for the shape of each value, typically a JSON Schema URL. Purely
	/// descriptive: a consumer that doesn't recognize it can still read the track.
	#[serde(default)]
	pub schema: Option<String>,

	/// The maximum bitrate of the track in bits per second, if known.
	#[serde(default)]
	pub bitrate: Option<u64>,

	/// The maximum delay between a payload being ready and the publisher flushing it, with the same
	/// meaning and whole-millisecond encoding as [`VideoConfig::jitter`](crate::catalog::VideoConfig::jitter).
	#[serde_as(as = "MillisCeil")]
	#[serde(default)]
	pub jitter: Option<std::time::Duration>,

	/// How far this track's payloads reach the transport behind the broadcast's earliest
	/// rendition, with the same meaning and encoding as
	/// [`VideoConfig::delay`](crate::catalog::VideoConfig::delay). Only measured for payloads that
	/// carry a capture time.
	#[serde_as(as = "MillisCeil")]
	#[serde(default)]
	pub delay: Option<std::time::Duration>,

	/// Fields this build doesn't recognize, kept so the entry round-trips.
	///
	/// A future [`Mode`] almost certainly comes with fields describing it, and
	/// preserving the discriminant without them would leave a relay republishing an entry nothing
	/// can act on. Same reason [`Container`](crate::catalog::Container) keeps an unknown container
	/// verbatim.
	#[serde(flatten)]
	pub extra: serde_json::Map<String, serde_json::Value>,
}

impl JsonConfig {
	/// A config for a track read in `mode`, with no optional fields set.
	pub fn new(mode: Mode) -> Self {
		Self {
			broadcast: None,
			track: None,
			epoch: None,
			mode,
			schema: None,
			bitrate: None,
			jitter: None,
			delay: None,
			extra: Default::default(),
		}
	}
}

/// The config itself, so a data producer takes it wherever it takes an entry embedding one.
impl AsMut<JsonConfig> for JsonConfig {
	fn as_mut(&mut self) -> &mut Self {
		self
	}
}
