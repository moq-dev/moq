use moq_pattern::{Pattern, Patterns};
use serde::de::{
	MapAccess, SeqAccess, Visitor,
	value::{MapAccessDeserializer, SeqAccessDeserializer},
};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A positive bitrate in bits per second, exactly representable in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Rate(u64);

/// A bitrate is zero or outside JSON's exact integer range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("rate must be a positive integer of at most 9007199254740991 bits per second")]
pub struct InvalidRate;

impl Rate {
	/// Build a positive rate in bits per second.
	pub fn from_bps(value: u64) -> Result<Self, InvalidRate> {
		if value == 0 || value > 9_007_199_254_740_991 {
			return Err(InvalidRate);
		}
		Ok(Self(value))
	}

	/// Return this rate in bits per second.
	pub fn as_bps(self) -> u64 {
		self.0
	}
}

impl Serialize for Rate {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_u64(self.0)
	}
}

impl<'de> Deserialize<'de> for Rate {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Self::from_bps(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
	}
}

/// Paths a role may access and its optional per-connection bitrate cap.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Access {
	/// Patterns this role may access, relative to its root.
	pub path: Patterns,
	/// Maximum bits per second for this direction, or no cap when absent.
	pub rate: Option<Rate>,
}

impl Access {
	/// Whether this role grants no paths.
	pub fn is_empty(&self) -> bool {
		self.path.is_empty()
	}

	pub(crate) fn is_default(&self) -> bool {
		self.path.is_empty() && self.rate.is_none()
	}
}

impl From<Pattern> for Access {
	fn from(path: Pattern) -> Self {
		Patterns::from(path).into()
	}
}

impl From<Patterns> for Access {
	fn from(path: Patterns) -> Self {
		Self { path, rate: None }
	}
}

impl From<Vec<Pattern>> for Access {
	fn from(path: Vec<Pattern>) -> Self {
		path.into_iter().collect()
	}
}

impl<const N: usize> From<[Pattern; N]> for Access {
	fn from(path: [Pattern; N]) -> Self {
		path.into_iter().collect()
	}
}

impl FromIterator<Pattern> for Access {
	fn from_iter<T: IntoIterator<Item = Pattern>>(iter: T) -> Self {
		Patterns::from_iter(iter).into()
	}
}

struct Paths(Patterns);

impl<'de> Deserialize<'de> for Paths {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		deserializer.deserialize_any(PathsVisitor)
	}
}

struct PathsVisitor;

impl<'de> Visitor<'de> for PathsVisitor {
	type Value = Paths;

	fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str("a path pattern or list of path patterns")
	}

	fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Paths, E> {
		value
			.parse::<Pattern>()
			.map(|path| Paths(path.into()))
			.map_err(E::custom)
	}

	fn visit_seq<A: SeqAccess<'de>>(self, value: A) -> Result<Paths, A::Error> {
		Patterns::deserialize(SeqAccessDeserializer::new(value)).map(Paths)
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Object {
	path: Paths,
	#[serde(default, deserialize_with = "crate::wire::present")]
	rate: Option<Rate>,
}

impl<'de> Deserialize<'de> for Access {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		// Dispatch by shape without an untagged fallback hiding pattern errors.
		deserializer.deserialize_any(AccessVisitor)
	}
}

struct AccessVisitor;

impl<'de> Visitor<'de> for AccessVisitor {
	type Value = Access;

	fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str("a path pattern, list, or access object")
	}

	fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Access, E> {
		PathsVisitor.visit_str(value).map(|path| path.0.into())
	}

	fn visit_seq<A: SeqAccess<'de>>(self, value: A) -> Result<Access, A::Error> {
		PathsVisitor.visit_seq(value).map(|path| path.0.into())
	}

	fn visit_map<A: MapAccess<'de>>(self, value: A) -> Result<Access, A::Error> {
		let Object { path, rate } = Object::deserialize(MapAccessDeserializer::new(value))?;
		Ok(Access { path: path.0, rate })
	}
}

impl Serialize for Access {
	fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self.rate {
			None => self.path.serialize(serializer),
			Some(rate) => {
				#[derive(Serialize)]
				struct Limited<'a> {
					path: &'a Patterns,
					rate: Rate,
				}
				Limited { path: &self.path, rate }.serialize(serializer)
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn refuses_malformed_or_lossy_caps() {
		for rate in ["0", "-1", "1.5", "9007199254740992", "null", "\"8000\""] {
			let json = format!(r#"{{"path":"**","rate":{rate}}}"#);
			assert!(serde_json::from_str::<Access>(&json).is_err(), "{json}");
		}
		assert!(serde_json::from_str::<Access>(r#"{"path":"**","raet":8000}"#).is_err());
	}

	#[test]
	fn shared_wire_vectors() {
		let vectors: serde_json::Value = serde_json::from_str(include_str!("../rate.json")).unwrap();
		for row in vectors["valid"].as_array().unwrap() {
			let claims: crate::Claims = serde_json::from_value(row["input"].clone()).unwrap();
			claims.validate().unwrap();
			assert_eq!(serde_json::to_value(&claims).unwrap(), row["claims"]);
			let roundtrip: crate::Claims = serde_json::from_value(row["claims"].clone()).unwrap();
			assert_eq!(roundtrip.publish, claims.publish);
			assert_eq!(roundtrip.subscribe, claims.subscribe);
			let grant: crate::Grant = serde_json::from_value(row["input"].clone()).unwrap();
			grant.validate().unwrap();
			assert_eq!(serde_json::to_value(&grant).unwrap(), row["grant"]);
		}
		for input in vectors["invalid"].as_array().unwrap() {
			assert!(
				serde_json::from_value::<crate::Claims>(input.clone()).is_err(),
				"{input}"
			);
			assert!(
				serde_json::from_value::<crate::Grant>(input.clone()).is_err(),
				"{input}"
			);
		}
	}

	#[test]
	fn caps_survive_signing_and_path_rebasing_but_scopes_refuse_them() {
		let wire = serde_json::json!({"root":"room","publish":{"path":"alice/**","rate":8000},"subscribe":{"path":"bob/**","rate":16000}});
		assert!(serde_json::from_value::<crate::Scope>(wire.clone()).is_err());
		let claims: crate::Claims = serde_json::from_value(wire).unwrap();
		let key = crate::Key::generate(crate::Algorithm::HS256, None).unwrap();
		let claims = key.verify(&key.sign(&claims).unwrap()).unwrap();
		for path in ["/", "room", "room/alice"] {
			let permissions = claims.authorize(path).unwrap();
			assert_eq!(permissions.publish.rate.unwrap().as_bps(), 8000, "{path}");
			assert_eq!(permissions.subscribe.rate.unwrap().as_bps(), 16000, "{path}");
		}
		let scope: crate::Scope = serde_json::from_str(r#"{"root":"room","publish":"**","subscribe":"**"}"#).unwrap();
		assert!(scope.allows(&claims), "key scopes bound paths, not bitrate");
	}
}
