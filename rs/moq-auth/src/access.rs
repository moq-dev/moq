use moq_pattern::{Pattern, Patterns};
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

	/// Whether these paths and this cap permit everything in `other`.
	pub fn covers(&self, other: &Self) -> bool {
		self.path.covers(&other.path)
			&& match (self.rate, other.rate) {
				(None, _) => true,
				(Some(a), Some(b)) => a >= b,
				(Some(_), None) => false,
			}
	}

	pub(crate) fn is_default(&self) -> bool {
		self.path.is_empty() && self.rate.is_none()
	}

	/// Combine path grants, keeping the higher cap or uncapped access.
	pub fn union(mut self, other: Self) -> Self {
		self.path.extend(other.path);
		self.rate = match (self.rate, other.rate) {
			(Some(a), Some(b)) => Some(a.max(b)),
			_ => None,
		};
		self
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

impl FromIterator<Pattern> for Access {
	fn from_iter<T: IntoIterator<Item = Pattern>>(iter: T) -> Self {
		Patterns::from_iter(iter).into()
	}
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Paths {
	One(Pattern),
	Many(Patterns),
}

impl Paths {
	fn into_patterns(self) -> Patterns {
		match self {
			Self::One(path) => path.into(),
			Self::Many(paths) => paths,
		}
	}
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Object {
	path: Paths,
	#[serde(default, deserialize_with = "crate::wire::present")]
	rate: Option<Rate>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Wire {
	Paths(Paths),
	Object(Object),
}

impl<'de> Deserialize<'de> for Access {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		Ok(match Wire::deserialize(deserializer)? {
			Wire::Paths(path) => path.into_patterns().into(),
			Wire::Object(Object { path, rate }) => Self {
				path: path.into_patterns(),
				rate,
			},
		})
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
	fn unions_choose_the_highest_cap_and_uncapped_lifts_it() {
		let access = |rate| Access {
			path: Pattern::all().into(),
			rate: Some(Rate::from_bps(rate).unwrap()),
		};
		assert_eq!(access(8000).union(access(16000)).rate.unwrap().as_bps(), 16000);
		let uncapped: Access = [Pattern::all()].into_iter().collect();
		assert_eq!(access(8000).union(uncapped.clone()).rate, None);
		assert_eq!(uncapped.union(access(8000)).rate, None);
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
