use std::collections::BTreeMap;

use serde::de::{DeserializeSeed, Deserializer, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor};

use crate::catalog::VERSION;

/// The catalog entries a reader skipped because a newer moq-hang draft defines them.
///
/// Each section maps a skipped entry's ID to its `version`. The rest of the catalog reads as if
/// those entries were absent, so a player plays what it can and may tell its user to upgrade for
/// the rest.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Skipped {
	/// Skipped video renditions.
	pub video: BTreeMap<String, u64>,
	/// Skipped audio renditions.
	pub audio: BTreeMap<String, u64>,
	/// Skipped text renditions.
	pub text: BTreeMap<String, u64>,
	/// Skipped JSON tracks.
	pub json: BTreeMap<String, u64>,
	/// Skipped binary tracks.
	pub binary: BTreeMap<String, u64>,
	/// Skipped includes.
	pub include: BTreeMap<String, u64>,
}

impl Skipped {
	fn sections(&self) -> [&BTreeMap<String, u64>; 6] {
		[
			&self.video,
			&self.audio,
			&self.text,
			&self.json,
			&self.binary,
			&self.include,
		]
	}

	/// Whether every entry was read.
	pub fn is_empty(&self) -> bool {
		self.sections().iter().all(|section| section.is_empty())
	}

	/// The draft a reader needs to read every skipped entry, or `None` if none was skipped.
	pub fn version(&self) -> Option<u64> {
		self.sections().into_iter().flat_map(BTreeMap::values).copied().max()
	}
}

/// What a reader checks before decoding any section: the root `version`, and which entries are too
/// new to read.
///
/// Read straight from the bytes where it can, so a repeated `version` member is refused rather
/// than resolved to either copy. Every other shape is left to the catalog's own decoding.
#[derive(Default)]
pub(crate) struct Probe {
	pub version: Option<u64>,
	pub skipped: Skipped,
}

impl<'de> serde::Deserialize<'de> for Probe {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Lenient(RootSeed).deserialize(deserializer)
	}
}

/// Reads one JSON object's members.
trait MapSeed {
	type Value: Default;
	fn visit<'de, A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error>;
}

/// Reads an object with its [`MapSeed`] and anything else as the seed's default.
struct Lenient<S>(S);

impl<'de, S: MapSeed> DeserializeSeed<'de> for Lenient<S> {
	type Value = S::Value;

	fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
		deserializer.deserialize_any(self)
	}
}

impl<'de, S: MapSeed> Visitor<'de> for Lenient<S> {
	type Value = S::Value;

	fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
		f.write_str("any JSON value")
	}

	fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
		self.0.visit(map)
	}

	fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
		while seq.next_element::<IgnoredAny>()?.is_some() {}
		Ok(Default::default())
	}

	fn visit_bool<E>(self, _: bool) -> Result<Self::Value, E> {
		Ok(Default::default())
	}

	fn visit_i64<E>(self, _: i64) -> Result<Self::Value, E> {
		Ok(Default::default())
	}

	fn visit_u64<E>(self, _: u64) -> Result<Self::Value, E> {
		Ok(Default::default())
	}

	fn visit_f64<E>(self, _: f64) -> Result<Self::Value, E> {
		Ok(Default::default())
	}

	fn visit_str<E>(self, _: &str) -> Result<Self::Value, E> {
		Ok(Default::default())
	}

	fn visit_unit<E>(self) -> Result<Self::Value, E> {
		Ok(Default::default())
	}
}

/// Reads a `version` member, refusing a second one.
fn version<'de, A: MapAccess<'de>>(map: &mut A, version: &mut Option<u64>) -> Result<(), A::Error> {
	if version.is_some() {
		return Err(A::Error::duplicate_field("version"));
	}
	*version = Some(map.next_value()?);
	Ok(())
}

struct RootSeed;

impl MapSeed for RootSeed {
	type Value = Probe;

	fn visit<'de, A: MapAccess<'de>>(self, mut map: A) -> Result<Probe, A::Error> {
		let mut probe = Probe::default();
		while let Some(key) = map.next_key::<String>()? {
			let skipped = &mut probe.skipped;
			let (section, entries) = match key.as_str() {
				"version" => {
					version(&mut map, &mut probe.version)?;
					continue;
				}
				"video" => (&mut skipped.video, Some("renditions")),
				"audio" => (&mut skipped.audio, Some("renditions")),
				"text" => (&mut skipped.text, Some("renditions")),
				"json" => (&mut skipped.json, Some("tracks")),
				"binary" => (&mut skipped.binary, Some("tracks")),
				"include" => (&mut skipped.include, None),
				_ => {
					map.next_value::<IgnoredAny>()?;
					continue;
				}
			};
			*section = match entries {
				Some(key) => map.next_value_seed(Lenient(SectionSeed(key)))?,
				None => map.next_value_seed(Lenient(EntriesSeed))?,
			};
		}
		Ok(probe)
	}
}

/// A section holding its entries under one key (`renditions`, `tracks`).
struct SectionSeed(&'static str);

impl MapSeed for SectionSeed {
	type Value = BTreeMap<String, u64>;

	fn visit<'de, A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
		let mut skipped = BTreeMap::new();
		while let Some(key) = map.next_key::<String>()? {
			if key == self.0 {
				skipped = map.next_value_seed(Lenient(EntriesSeed))?;
			} else {
				map.next_value::<IgnoredAny>()?;
			}
		}
		Ok(skipped)
	}
}

/// A map of entries keyed by ID, yielding the ones newer than this reader.
struct EntriesSeed;

impl MapSeed for EntriesSeed {
	type Value = BTreeMap<String, u64>;

	fn visit<'de, A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
		let mut skipped = BTreeMap::new();
		while let Some(id) = map.next_key::<String>()? {
			if let Some(version) = map.next_value_seed(Lenient(EntrySeed))?
				&& version > VERSION
			{
				skipped.insert(id, version);
			}
		}
		Ok(skipped)
	}
}

/// One entry, yielding its `version`.
struct EntrySeed;

impl MapSeed for EntrySeed {
	type Value = Option<u64>;

	fn visit<'de, A: MapAccess<'de>>(self, mut map: A) -> Result<Option<u64>, A::Error> {
		let mut entry = None;
		while let Some(key) = map.next_key::<String>()? {
			if key == "version" {
				version(&mut map, &mut entry)?;
			} else {
				map.next_value::<IgnoredAny>()?;
			}
		}
		Ok(entry)
	}
}
