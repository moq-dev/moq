//! The JSON encoding of the grants in [`Claims`](crate::Claims) and [`Scope`](crate::Scope).
//!
//! Grants are written as `publish` and `subscribe` pattern lists. They were once prefix
//! lists named `put` and `get`, which are still read: a prefix `p` means exactly the
//! pattern `p/**` (and `""` means `**`). One document never mixes the two encodings.
//!
//! A role with a bitrate cap is written as an object, `{"path": [...], "rate": bps}`,
//! which every older reader refuses rather than drop the cap.

use crate::{Access, Link};
use moq_pattern::{Pattern, Patterns};
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{DurationSeconds, serde_as};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::time::Duration;

/// The wire form of [`Claims`](crate::Claims).
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Claims {
	#[serde(skip_serializing_if = "String::is_empty")]
	root: String,
	// The legacy grants, read and never written.
	#[serde(deserialize_with = "present", skip_serializing)]
	put: Option<Prefixes>,
	#[serde(deserialize_with = "present", skip_serializing)]
	get: Option<Prefixes>,
	#[serde(deserialize_with = "present")]
	publish: Option<Access>,
	#[serde(deserialize_with = "present")]
	subscribe: Option<Access>,
	#[serde(with = "seconds", skip_serializing_if = "Option::is_none")]
	exp: Option<kio::clock::Wall>,
	#[serde(with = "seconds", skip_serializing_if = "Option::is_none")]
	iat: Option<kio::clock::Wall>,
	#[serde(
		serialize_with = "seconds::serialize_ceil",
		deserialize_with = "seconds::deserialize",
		skip_serializing_if = "Option::is_none"
	)]
	nbf: Option<kio::clock::Wall>,
	#[serde(deserialize_with = "present")]
	jti: Option<crate::TokenId>,
	// Registered claims that narrow nothing: an issuer's bookkeeping, read and dropped.
	// Every other claim is refused, since an unknown one may narrow the grant, and a
	// misspelled `root` would otherwise widen it to everything.
	#[serde(skip_serializing)]
	iss: Option<serde::de::IgnoredAny>,
	#[serde(skip_serializing)]
	sub: Option<serde::de::IgnoredAny>,
}

impl From<crate::Claims> for Claims {
	fn from(claims: crate::Claims) -> Self {
		Self {
			root: claims.root,
			put: None,
			get: None,
			publish: written(claims.publish),
			subscribe: written(claims.subscribe),
			exp: claims.expires,
			iat: claims.issued,
			nbf: claims.not_before,
			jti: claims.id,
			iss: None,
			sub: None,
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
			publish,
			subscribe,
			expires: wire.exp,
			issued: wire.iat,
			not_before: wire.nbf,
			id: wire.jti,
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
	// The legacy grants, read and never written.
	#[serde(deserialize_with = "present", skip_serializing)]
	put: Option<Vec<String>>,
	#[serde(deserialize_with = "present", skip_serializing)]
	get: Option<Vec<String>>,
	#[serde(deserialize_with = "present")]
	publish: Option<Patterns>,
	#[serde(deserialize_with = "present")]
	subscribe: Option<Patterns>,
}

impl From<crate::Scope> for Scope {
	fn from(scope: crate::Scope) -> Self {
		let paths = |patterns: Patterns| (!patterns.is_empty()).then_some(patterns);
		Self {
			root: scope.root,
			put: None,
			get: None,
			publish: paths(scope.publish),
			subscribe: paths(scope.subscribe),
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
			publish: wire.publish.map(Access::new),
			subscribe: wire.subscribe.map(Access::new),
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
#[derive(Deserialize)]
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

/// A wire struct read from a JSON object only. Serde's derive also reads a struct from a
/// positional array, which would fill the first fields and default the rest, while `@moq/auth`
/// refuses anything but an object.
pub(crate) struct Object<T>(T);

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Object<T> {
	fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		struct Visitor<T>(PhantomData<T>);

		impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Visitor<T> {
			type Value = Object<T>;

			fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
				formatter.write_str("an object")
			}

			fn visit_map<A: serde::de::MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
				T::deserialize(serde::de::value::MapAccessDeserializer::new(map)).map(Object)
			}
		}

		deserializer.deserialize_map(Visitor(PhantomData))
	}
}

impl TryFrom<Object<Claims>> for crate::Claims {
	type Error = String;

	fn try_from(Object(wire): Object<Claims>) -> Result<Self, Self::Error> {
		wire.try_into()
	}
}

impl TryFrom<Object<Scope>> for crate::Scope {
	type Error = String;

	fn try_from(Object(wire): Object<Scope>) -> Result<Self, Self::Error> {
		wire.try_into()
	}
}

impl TryFrom<Object<Grant>> for crate::Grant {
	type Error = String;

	fn try_from(Object(wire): Object<Grant>) -> Result<Self, Self::Error> {
		wire.try_into()
	}
}

/// A present field, refusing an explicit `null` rather than reading it as absent,
/// so a `null` can't hide one encoding from the mixed-encoding check.
pub(crate) fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
	T::deserialize(deserializer).map(Some)
}

/// A present `revalidate` in whole seconds, refusing `null` as [`present`] does.
mod revalidate {
	use super::*;

	pub(super) fn serialize<S: serde::Serializer>(value: &Option<Duration>, serializer: S) -> Result<S::Ok, S::Error> {
		match value {
			Some(value) => serializer.serialize_u64(value.as_secs()),
			None => serializer.serialize_none(),
		}
	}

	pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Duration>, D::Error> {
		<DurationSeconds<u64> as serde_with::DeserializeAs<'de, Duration>>::deserialize_as(deserializer).map(Some)
	}
}

/// A present time in whole unix seconds, from the epoch to the clock's last second, which
/// `@moq/auth` holds exactly too. Anything else is refused rather than saturated, and an
/// explicit `null` rather than read as absent, which for `exp` would mean never.
mod seconds {
	use kio::clock::Wall;
	use serde::{Deserialize, Deserializer, Serializer, de::Error};

	/// Truncated, so the clock's last time writes as a second this reads back, and an
	/// expiry never moves later.
	pub(super) fn serialize<S: Serializer>(wall: &Option<Wall>, serializer: S) -> Result<S::Ok, S::Error> {
		match wall {
			Some(wall) => serializer.serialize_u64(wall.saturating_duration_since(Wall::UNIX_EPOCH).as_secs()),
			None => serializer.serialize_none(),
		}
	}

	/// Rounded up, so a not-before never moves earlier; past the clock's last second it
	/// writes a time this refuses to read.
	pub(super) fn serialize_ceil<S: Serializer>(wall: &Option<Wall>, serializer: S) -> Result<S::Ok, S::Error> {
		match wall {
			Some(wall) => {
				let since = wall.saturating_duration_since(Wall::UNIX_EPOCH);
				let seconds = since.as_secs() + u64::from(since.subsec_nanos() > 0);
				serializer.serialize_u64(seconds)
			}
			None => serializer.serialize_none(),
		}
	}

	pub(super) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Wall>, D::Error> {
		let seconds = u64::deserialize(deserializer)?;
		Wall::UNIX_EPOCH
			.checked_add(std::time::Duration::from_secs(seconds))
			.map(Some)
			.ok_or_else(|| D::Error::custom(format!("{seconds} is past the last representable time")))
	}
}

/// A role as written: omitted when it grants nothing and carries no cap.
fn written(role: Access) -> Option<Access> {
	(!role.is_unset()).then_some(role)
}

/// The grant fields shared by both documents, in whichever encoding they arrived.
struct Grants {
	put: Option<Vec<String>>,
	get: Option<Vec<String>>,
	publish: Option<Access>,
	subscribe: Option<Access>,
}

impl Grants {
	fn decode(self) -> Result<(Access, Access), String> {
		let legacy = self.put.is_some() || self.get.is_some();
		if !legacy {
			return Ok((self.publish.unwrap_or_default(), self.subscribe.unwrap_or_default()));
		}
		if self.publish.is_some() || self.subscribe.is_some() {
			return Err("mixes the legacy put/get fields with publish/subscribe".to_string());
		}

		let subtrees = |prefixes: Option<Vec<String>>| -> Result<Access, String> {
			let path: Patterns = prefixes
				.unwrap_or_default()
				.iter()
				.map(|prefix| Pattern::subtree(prefix).map_err(|err| format!("legacy prefix {prefix:?}: {err}")))
				.collect::<Result<_, _>>()?;
			Ok(Access::new(path))
		};
		Ok((subtrees(self.put)?, subtrees(self.get)?))
	}
}

/// The wire form of [`Grant`](crate::Grant). A field it does not know is refused, as in
/// claims: the grant is what the relay enforces, and a field added later may narrow it.
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Grant {
	#[serde(skip_serializing_if = "Access::is_unset")]
	publish: Access,
	#[serde(skip_serializing_if = "Access::is_unset")]
	subscribe: Access,
	#[serde(deserialize_with = "present")]
	root: Option<String>,
	#[serde(skip_serializing_if = "BTreeMap::is_empty")]
	mounts: BTreeMap<String, String>,
	#[serde(with = "seconds", skip_serializing_if = "Option::is_none")]
	expires: Option<kio::clock::Wall>,
	#[serde(with = "revalidate", skip_serializing_if = "Option::is_none")]
	revalidate: Option<Duration>,
	#[serde(deserialize_with = "present")]
	tier: Option<String>,
	#[serde(deserialize_with = "present")]
	label: Option<String>,
	#[serde(deserialize_with = "present")]
	link: Option<Link>,
	// TEMPORARY(0.22): the boolean every relay before 0.22 reads in place of `link`.
	// Delete it at the next version migration, once no relay that predates `link` runs.
	#[serde(deserialize_with = "present")]
	peer: Option<bool>,
}

impl From<crate::Grant> for Grant {
	fn from(grant: crate::Grant) -> Self {
		Self {
			publish: grant.publish,
			subscribe: grant.subscribe,
			root: grant.root,
			mounts: grant.mounts,
			expires: grant.expires,
			revalidate: grant.revalidate,
			tier: grant.tier,
			label: grant.label,
			link: grant.link,
			// TEMPORARY(0.22): every link is a peer to a relay that predates `link`.
			peer: grant.link.map(|_| true),
		}
	}
}

impl TryFrom<Grant> for crate::Grant {
	type Error = String;

	fn try_from(wire: Grant) -> Result<Self, Self::Error> {
		// TEMPORARY(0.22): a bare `peer` is the link a relay before 0.22 reads.
		let link = match (wire.link, wire.peer) {
			(link, None) => link,
			(None, Some(peer)) => peer.then_some(Link::Peer),
			(Some(link), Some(true)) => Some(link),
			(Some(link), Some(false)) => return Err(format!("`link` is {link} but `peer` is false")),
		};
		Ok(Self {
			publish: wire.publish,
			subscribe: wire.subscribe,
			root: wire.root,
			mounts: wire.mounts,
			expires: wire.expires,
			revalidate: wire.revalidate,
			tier: wire.tier,
			label: wire.label,
			link,
		})
	}
}
