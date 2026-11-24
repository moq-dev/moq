use crate::Rate;
use moq_pattern::Prefixes;
use serde::{Deserialize, Deserializer, Serialize};

/// What one role (publish or subscribe) of a token or grant reaches: path prefixes,
/// relative to the root, each covering its path and everything beneath it, and an
/// optional bitrate cap.
///
/// On the wire it is the list of prefixes, or `{"path": [...], "rate": bps}` when
/// capped, which every older reader refuses rather than drop the cap. The object takes
/// exactly `path` and `rate`, so a misspelled cap is refused rather than read as none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Access {
	/// Prefixes the role may reach, relative to the root.
	pub path: Prefixes,

	/// The most the role may move, or no cap when absent.
	pub rate: Option<Rate>,
}

impl Access {
	/// Access to these prefixes, uncapped.
	pub fn new(path: impl Into<Prefixes>) -> Self {
		Self {
			path: path.into(),
			rate: None,
		}
	}

	/// Cap the role in bits per second; `None` lifts the cap.
	pub fn with_rate(mut self, rate: impl Into<Option<Rate>>) -> Self {
		self.rate = rate.into();
		self
	}

	/// Whether the role reaches nothing and carries no cap, so the wire omits it.
	pub(crate) fn is_unset(&self) -> bool {
		self.path.is_empty() && self.rate.is_none()
	}
}

impl From<Prefixes> for Access {
	fn from(path: Prefixes) -> Self {
		Self::new(path)
	}
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capped<P> {
	path: P,
	rate: Rate,
}

impl Access {
	/// Write the role in its wire shape, with `path` standing in for its prefix list.
	pub(crate) fn serialize_as<P: Serialize, S: serde::Serializer>(
		&self,
		path: P,
		serializer: S,
	) -> Result<S::Ok, S::Error> {
		match self.rate {
			None => path.serialize(serializer),
			Some(rate) => Capped { path, rate }.serialize(serializer),
		}
	}

	/// Read the role from its wire shape, its prefix list read as `P`.
	pub(crate) fn deserialize_as<'de, P, D>(deserializer: D) -> Result<Self, D::Error>
	where
		P: Deserialize<'de> + Into<Prefixes>,
		D: Deserializer<'de>,
	{
		use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};
		use std::marker::PhantomData;

		// Dispatch on the shape, so a bad prefix or cap reports its own error rather
		// than an untagged enum's "matched no variant".
		struct Visitor<P>(PhantomData<P>);
		impl<'de, P: Deserialize<'de> + Into<Prefixes>> serde::de::Visitor<'de> for Visitor<P> {
			type Value = Access;

			fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
				f.write_str("a list of path prefixes, or an object of a `path` list and a `rate`")
			}

			fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Access, A::Error> {
				Ok(Access::new(P::deserialize(SeqAccessDeserializer::new(seq))?))
			}

			fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Access, A::Error> {
				let Capped { path, rate } = Capped::<P>::deserialize(MapAccessDeserializer::new(map))?;
				Ok(Access::new(path).with_rate(rate))
			}
		}
		deserializer.deserialize_any(Visitor::<P>(PhantomData))
	}
}

impl Serialize for Access {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		self.serialize_as(&self.path, serializer)
	}
}

impl<'de> Deserialize<'de> for Access {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Self::deserialize_as::<Prefixes, D>(deserializer)
	}
}
