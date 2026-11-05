//! The JSON encoding of the grants in [`Claims`](crate::Claims) and [`Scope`](crate::Scope).
//!
//! Grants were once prefix lists named `put` and `get`, and every published `moq-token`
//! reader still expects them. A prefix `p` means exactly the pattern `p/**` (and `""`
//! means `**`), so grants that are all subtrees are written the old way and every
//! reader, old or new, agrees on what they grant. Anything a prefix can't say is written
//! as `publish` and `subscribe`, which an old reader sees as granting nothing and
//! refuses. Both encodings are read; one document never mixes them.
//!
//! A role with a bitrate cap is written as an object, `{"path": [...], "rate": bps}`,
//! which every older reader refuses rather than drop the cap, so a capped role is never
//! written as prefixes.

use crate::Rate;
use moq_pattern::{Pattern, Patterns};
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{DurationSeconds, serde_as};
use std::collections::BTreeMap;
use std::time::Duration;

/// The wire form of [`Claims`](crate::Claims).
#[serde_with::skip_serializing_none]
#[serde_as]
#[derive(Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Claims {
	#[serde(skip_serializing_if = "String::is_empty")]
	root: String,
	#[serde(deserialize_with = "present")]
	put: Option<Prefixes>,
	#[serde(deserialize_with = "present")]
	get: Option<Prefixes>,
	#[serde(deserialize_with = "present")]
	publish: Option<Role>,
	#[serde(deserialize_with = "present")]
	subscribe: Option<Role>,
	#[serde_as(as = "Option<kio::clock::serde::UnixSeconds>")]
	exp: Option<kio::clock::Wall>,
	#[serde_as(as = "Option<kio::clock::serde::UnixSeconds>")]
	iat: Option<kio::clock::Wall>,
	#[serde_as(as = "Option<kio::clock::serde::UnixSeconds>")]
	nbf: Option<kio::clock::Wall>,
	// Registered claims that narrow nothing: an issuer's bookkeeping, read and dropped.
	// Every other claim is refused, since an unknown one may narrow the grant, and a
	// misspelled `root` would otherwise widen it to everything.
	#[serde(skip_serializing)]
	iss: Option<serde::de::IgnoredAny>,
	#[serde(skip_serializing)]
	sub: Option<serde::de::IgnoredAny>,
	#[serde(skip_serializing)]
	jti: Option<serde::de::IgnoredAny>,
}

impl From<crate::Claims> for Claims {
	fn from(claims: crate::Claims) -> Self {
		let grants = Grants::encode(
			Role::new(claims.publish, claims.publish_rate),
			Role::new(claims.subscribe, claims.subscribe_rate),
		);
		Self {
			root: claims.root,
			put: grants.put.map(Prefixes::Many),
			get: grants.get.map(Prefixes::Many),
			publish: grants.publish,
			subscribe: grants.subscribe,
			exp: claims.expires,
			iat: claims.issued,
			nbf: claims.not_before,
			iss: None,
			sub: None,
			jti: None,
		}
	}
}

impl TryFrom<Claims> for crate::Claims {
	type Error = String;

	fn try_from(wire: Claims) -> Result<Self, Self::Error> {
		let grants = Grants {
			put: wire.put.map(Prefixes::into_vec),
			get: wire.get.map(Prefixes::into_vec),
			publish: wire.publish,
			subscribe: wire.subscribe,
		};
		let (publish, subscribe) = grants.decode()?;
		Ok(Self {
			root: wire.root,
			publish: publish.path,
			publish_rate: publish.rate,
			subscribe: subscribe.path,
			subscribe_rate: subscribe.rate,
			expires: wire.exp,
			issued: wire.iat,
			not_before: wire.nbf,
		})
	}
}

/// The wire form of [`Scope`](crate::Scope). Legacy scopes only ever held lists.
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Scope {
	#[serde(skip_serializing_if = "String::is_empty")]
	root: String,
	#[serde(deserialize_with = "present")]
	put: Option<Vec<String>>,
	#[serde(deserialize_with = "present")]
	get: Option<Vec<String>>,
	#[serde(deserialize_with = "present")]
	publish: Option<Patterns>,
	#[serde(deserialize_with = "present")]
	subscribe: Option<Patterns>,
}

impl From<crate::Scope> for Scope {
	fn from(scope: crate::Scope) -> Self {
		let grants = Grants::encode(Role::new(scope.publish, None), Role::new(scope.subscribe, None));
		let paths = |role: Option<Role>| role.map(|role| role.path);
		Self {
			root: scope.root,
			put: grants.put,
			get: grants.get,
			publish: paths(grants.publish),
			subscribe: paths(grants.subscribe),
		}
	}
}

impl TryFrom<Scope> for crate::Scope {
	type Error = String;

	fn try_from(wire: Scope) -> Result<Self, Self::Error> {
		// A key scope bounds paths, never bitrate, so its roles are only lists.
		let grants = Grants {
			put: wire.put,
			get: wire.get,
			publish: wire.publish.map(|path| Role::new(path, None)),
			subscribe: wire.subscribe.map(|path| Role::new(path, None)),
		};
		let (publish, subscribe) = grants.decode()?;
		Ok(Self {
			root: wire.root,
			publish: publish.path,
			subscribe: subscribe.path,
		})
	}
}

/// Legacy claims wrote a single prefix as a bare string.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Prefixes {
	One(String),
	Many(Vec<String>),
}

impl Prefixes {
	fn into_vec(self) -> Vec<String> {
		match self {
			Self::One(prefix) => vec![prefix],
			Self::Many(prefixes) => prefixes,
		}
	}
}

/// A present field, refusing an explicit `null` rather than reading it as absent,
/// so a `null` can't hide one encoding from the mixed-encoding check.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
	T::deserialize(deserializer).map(Some)
}

/// The grant fields shared by both documents, in whichever encoding they arrived.
struct Grants {
	put: Option<Vec<String>>,
	get: Option<Vec<String>>,
	publish: Option<Role>,
	subscribe: Option<Role>,
}

impl Grants {
	fn encode(publish: Role, subscribe: Role) -> Self {
		// A cap has no prefix form, so one moves the whole document to patterns.
		let prefixes = |role: &Role| -> Option<Vec<String>> {
			if role.rate.is_some() {
				return None;
			}
			role.path
				.iter()
				.map(|pattern| pattern.as_prefix().map(str::to_string))
				.collect()
		};
		let some = |role: Role| (!role.is_empty()).then_some(role);

		match (prefixes(&publish), prefixes(&subscribe)) {
			(Some(put), Some(get)) => Self {
				put: (!put.is_empty()).then_some(put),
				get: (!get.is_empty()).then_some(get),
				publish: None,
				subscribe: None,
			},
			_ => Self {
				put: None,
				get: None,
				publish: some(publish),
				subscribe: some(subscribe),
			},
		}
	}

	fn decode(self) -> Result<(Role, Role), String> {
		let legacy = self.put.is_some() || self.get.is_some();
		if !legacy {
			return Ok((self.publish.unwrap_or_default(), self.subscribe.unwrap_or_default()));
		}
		if self.publish.is_some() || self.subscribe.is_some() {
			return Err("mixes the legacy put/get fields with publish/subscribe".to_string());
		}

		let subtrees = |prefixes: Option<Vec<String>>| -> Result<Role, String> {
			let path = prefixes
				.unwrap_or_default()
				.iter()
				.map(|prefix| Pattern::subtree(prefix).map_err(|err| format!("legacy prefix {prefix:?}: {err}")))
				.collect::<Result<_, _>>()?;
			Ok(Role::new(path, None))
		};
		Ok((subtrees(self.put)?, subtrees(self.get)?))
	}
}

/// One role on the wire: a list of patterns, or an object adding a bitrate cap.
///
/// The object takes exactly `path` and `rate`, so a misspelled cap is refused rather
/// than read as none.
#[derive(Default)]
struct Role {
	path: Patterns,
	rate: Option<Rate>,
}

impl Role {
	fn new(path: Patterns, rate: Option<Rate>) -> Self {
		Self { path, rate }
	}

	fn is_empty(&self) -> bool {
		self.path.is_empty() && self.rate.is_none()
	}
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Capped<P> {
	path: P,
	rate: Rate,
}

impl Serialize for Role {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		match self.rate {
			None => self.path.serialize(serializer),
			Some(rate) => Capped { path: &self.path, rate }.serialize(serializer),
		}
	}
}

impl<'de> Deserialize<'de> for Role {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		use serde::de::value::{MapAccessDeserializer, SeqAccessDeserializer};

		// Dispatch on the shape, so a bad pattern or cap reports its own error rather
		// than an untagged enum's "matched no variant".
		struct Visitor;
		impl<'de> serde::de::Visitor<'de> for Visitor {
			type Value = Role;

			fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
				f.write_str("a list of path patterns, or an object of a `path` list and a `rate`")
			}

			fn visit_seq<A: serde::de::SeqAccess<'de>>(self, seq: A) -> Result<Role, A::Error> {
				Ok(Role::new(Patterns::deserialize(SeqAccessDeserializer::new(seq))?, None))
			}

			fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Role, A::Error> {
				let Capped { path, rate } = Capped::<Patterns>::deserialize(MapAccessDeserializer::new(map))?;
				Ok(Role::new(path, Some(rate)))
			}
		}
		deserializer.deserialize_any(Visitor)
	}
}

/// The wire form of [`Grant`](crate::Grant), whose roles may carry caps. A field it does
/// not know is ignored, so an auth server may answer more than this reader takes.
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct Grant {
	#[serde(skip_serializing_if = "Role::is_empty")]
	publish: Role,
	#[serde(skip_serializing_if = "Role::is_empty")]
	subscribe: Role,
	root: Option<String>,
	#[serde(skip_serializing_if = "BTreeMap::is_empty")]
	mounts: BTreeMap<String, String>,
	#[serde_as(as = "Option<kio::clock::serde::UnixSeconds>")]
	expires: Option<kio::clock::Wall>,
	#[serde_as(as = "Option<DurationSeconds<u64>>")]
	revalidate: Option<Duration>,
	tier: Option<String>,
	label: Option<String>,
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	peer: bool,
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	upstream: bool,
}

impl From<crate::Grant> for Grant {
	fn from(grant: crate::Grant) -> Self {
		Self {
			publish: Role::new(grant.publish, grant.publish_rate),
			subscribe: Role::new(grant.subscribe, grant.subscribe_rate),
			root: grant.root,
			mounts: grant.mounts,
			expires: grant.expires,
			revalidate: grant.revalidate,
			tier: grant.tier,
			label: grant.label,
			peer: grant.peer,
			upstream: grant.upstream,
		}
	}
}

impl From<Grant> for crate::Grant {
	fn from(wire: Grant) -> Self {
		Self {
			publish: wire.publish.path,
			publish_rate: wire.publish.rate,
			subscribe: wire.subscribe.path,
			subscribe_rate: wire.subscribe.rate,
			root: wire.root,
			mounts: wire.mounts,
			expires: wire.expires,
			revalidate: wire.revalidate,
			tier: wire.tier,
			label: wire.label,
			peer: wire.peer,
			upstream: wire.upstream,
		}
	}
}
