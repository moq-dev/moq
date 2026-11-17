use crate::Rate;
use moq_pattern::Patterns;
use serde::{Deserialize, Deserializer, Serialize};

/// What one role (publish or subscribe) of a token or grant reaches: path patterns,
/// relative to the root, and an optional bitrate cap.
///
/// On the wire it is the list of patterns, or `{"path": [...], "rate": bps}` when
/// capped, which every older reader refuses rather than drop the cap. The object takes
/// exactly `path` and `rate`, so a misspelled cap is refused rather than read as none.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Access {
	/// Patterns the role may reach, relative to the root.
	pub path: Patterns,

	/// The most the role may move, or no cap when absent.
	pub rate: Option<Rate>,
}

impl Access {
	/// Access to these patterns, uncapped.
	pub fn new(path: impl Into<Patterns>) -> Self {
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

impl From<Patterns> for Access {
	fn from(path: Patterns) -> Self {
		Self::new(path)
	}
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capped<P> {
	path: P,
	rate: Rate,
}

impl Serialize for Access {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self.rate {
			None => self.path.serialize(serializer),
			Some(rate) => Capped { path: &self.path, rate }.serialize(serializer),
		}
	}
}

impl<'de> Deserialize<'de> for Access {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};

		// Dispatch on the shape, so a bad pattern or cap reports its own error rather
		// than an untagged enum's "matched no variant".
		struct Visitor;
		impl<'de> serde::de::Visitor<'de> for Visitor {
			type Value = Access;

			fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
				f.write_str("a list of path patterns, or an object of a `path` list and a `rate`")
			}

			fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Access, A::Error> {
				Ok(Access::new(Patterns::deserialize(SeqAccessDeserializer::new(seq))?))
			}

			fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Access, A::Error> {
				let Capped { path, rate } = Capped::<Patterns>::deserialize(MapAccessDeserializer::new(map))?;
				Ok(Access::new(path).with_rate(rate))
			}
		}
		deserializer.deserialize_any(Visitor)
	}
}
