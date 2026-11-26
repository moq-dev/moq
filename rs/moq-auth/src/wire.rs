//! The JSON encoding of the grants in [`Claims`](crate::Claims) and [`Scope`](crate::Scope).
//!
//! Grants are written as `publish` and `subscribe` prefix lists. They were once named
//! `put` and `get`, which are still read as the same prefixes, with their slashes
//! normalized as they always were. One document never mixes the two encodings.
//!
//! A role with a bitrate cap is written as an object, `{"path": [...], "rate": bps}`,
//! which every older reader refuses rather than drop the cap.
//!
//! Until the version migrations [`alias`] names, a prefix is written and read as the
//! subtree pattern releases before 0.22 spell it: `foo/**`, and `**` for the empty prefix.

use crate::{Access, Link};
use moq_pattern::{InvalidPrefix, Pattern, Prefix, Prefixes};
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
	put: Option<Legacy>,
	#[serde(deserialize_with = "present", skip_serializing)]
	get: Option<Legacy>,
	#[serde(deserialize_with = "present")]
	publish: Option<alias::Role>,
	#[serde(deserialize_with = "present")]
	subscribe: Option<alias::Role>,
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
			publish: written(claims.publish).map(alias::Role),
			subscribe: written(claims.subscribe).map(alias::Role),
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
			put: wire.put.map(Legacy::into_vec),
			get: wire.get.map(Legacy::into_vec),
			publish: wire.publish.map(|role| role.0),
			subscribe: wire.subscribe.map(|role| role.0),
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
	publish: Option<alias::List>,
	#[serde(deserialize_with = "present")]
	subscribe: Option<alias::List>,
}

impl From<crate::Scope> for Scope {
	fn from(scope: crate::Scope) -> Self {
		let paths = |prefixes: Prefixes| (!prefixes.is_empty()).then_some(alias::List(prefixes));
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
			publish: wire.publish.map(|list| Access::new(list.0)),
			subscribe: wire.subscribe.map(|list| Access::new(list.0)),
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
enum Legacy {
	One(String),
	Many(Vec<String>),
}

impl Legacy {
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
			// Rounded to the nearest second, as `DurationSeconds` writes it. A cadence of at
			// least `MIN_REVALIDATE` never rounds to the zero the wire can't carry.
			Some(value) => <DurationSeconds<u64> as serde_with::SerializeAs<Duration>>::serialize_as(value, serializer),
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

		// A legacy prefix had its slashes trimmed and collapsed, as a broadcast path does.
		let legacy = |prefix: &String| -> Result<Prefix, String> {
			let normalized = Pattern::literal(prefix).map_err(|err| format!("legacy prefix {prefix:?}: {err}"))?;
			Prefix::try_from(normalized.as_str()).map_err(|err| format!("legacy prefix {prefix:?}: {err}"))
		};
		let prefixes = |prefixes: Option<Vec<String>>| -> Result<Access, String> {
			let path: Prefixes = prefixes
				.unwrap_or_default()
				.iter()
				.map(legacy)
				.collect::<Result<_, _>>()?;
			Ok(Access::new(path))
		};
		Ok((prefixes(self.put)?, prefixes(self.get)?))
	}
}

/// The wire form of [`Grant`](crate::Grant). A field it does not know is refused, as in
/// claims: the grant is what the relay enforces, and a field added later may narrow it.
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Grant {
	#[serde(skip_serializing_if = "alias::Role::is_unset")]
	publish: alias::Role,
	#[serde(skip_serializing_if = "alias::Role::is_unset")]
	subscribe: alias::Role,
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
	// Zero refuses the grant, as `NonZeroU64` reads it.
	#[serde(deserialize_with = "present")]
	cost: Option<std::num::NonZeroU64>,
}

impl From<crate::Grant> for Grant {
	fn from(grant: crate::Grant) -> Self {
		Self {
			publish: alias::Role(grant.publish),
			subscribe: alias::Role(grant.subscribe),
			root: grant.root,
			mounts: grant.mounts,
			expires: grant.expires,
			revalidate: grant.revalidate,
			tier: grant.tier,
			label: grant.label,
			link: grant.link,
			// TEMPORARY(0.22): every link is a peer to a relay that predates `link`.
			peer: grant.link.map(|_| true),
			cost: grant.cost,
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
			publish: wire.publish.0,
			subscribe: wire.subscribe.0,
			root: wire.root,
			mounts: wire.mounts,
			expires: wire.expires,
			revalidate: wire.revalidate,
			tier: wire.tier,
			label: wire.label,
			link,
			cost: wire.cost,
		})
	}
}

/// TEMPORARY MIGRATION: grant lists as releases before 0.22 spell them.
///
/// Those releases read a member as a pattern: a bare `foo` is the one path `foo`, and `""`
/// the root alone. So every list one of them reads, in a token, a key scope, or an auth
/// server's grant, is written as the same subtrees in pattern form, `foo/**` and `**`, and
/// a member spelled that way reads back as its prefix. Any other wildcard is still
/// refused, and nothing else takes the alias: configs, flags, origin scopes, and AUTH_OK
/// stay strict. `@moq/auth`'s `wire.ts` mirrors this.
///
/// Deleted in two steps (`quest/m4/grant-alias-delete.md`): the first
/// version migration after 0.22 writes bare prefixes, and the one after it deletes the
/// read, once tokens and key scopes minted with `/**` have expired or been re-minted.
pub(crate) mod alias {
	use super::*;

	/// The prefix a member names: `foo/**` is `foo`, `**` the empty prefix, and anything
	/// else is parsed as a prefix.
	pub(crate) fn read(text: &str) -> Result<Prefix, InvalidPrefix> {
		let head = match text.strip_suffix("/**") {
			Some(head) if !head.is_empty() => head,
			_ if text == "**" => "",
			_ => text,
		};
		// A wildcard left in the head names something other than one subtree.
		if head.contains('*') {
			return Err(InvalidPrefix::Wildcard(text.to_string()));
		}
		head.parse()
	}

	/// Prefixes spelled as the subtree patterns they cover. A prefix as deep as a path may
	/// be is its literal, which a release reads as that one path: nothing lies beneath it.
	struct Written<'a>(&'a Prefixes);

	impl Serialize for Written<'_> {
		fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
			serializer.collect_seq(self.0.iter().map(Prefix::pattern))
		}
	}

	/// A key scope's prefix list.
	pub(crate) struct List(pub(crate) Prefixes);

	impl From<List> for Prefixes {
		fn from(list: List) -> Self {
			list.0
		}
	}

	impl Serialize for List {
		fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
			Written(&self.0).serialize(serializer)
		}
	}

	impl<'de> Deserialize<'de> for List {
		fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
			let texts = Vec::<String>::deserialize(deserializer)?;
			let prefixes = texts.iter().map(|text| read(text)).collect::<Result<_, _>>();
			prefixes.map(List).map_err(serde::de::Error::custom)
		}
	}

	/// A token's or grant's role.
	#[derive(Default)]
	pub(crate) struct Role(pub(crate) Access);

	impl Role {
		pub(crate) fn is_unset(&self) -> bool {
			self.0.is_unset()
		}
	}

	impl Serialize for Role {
		fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
			self.0.serialize_as(Written(&self.0.path), serializer)
		}
	}

	impl<'de> Deserialize<'de> for Role {
		fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
			Access::deserialize_as::<List, D>(deserializer).map(Role)
		}
	}

	#[cfg(test)]
	mod tests {
		use super::*;

		#[test]
		fn reads_subtree_patterns_as_their_prefix() {
			for (text, prefix) in [
				("**", ""),
				("foo/**", "foo"),
				("foo/bar/**", "foo/bar"),
				(r"a\/b/**", r"a\/b"),
				(r"a\\/**", r"a\\"),
				("foo", "foo"),
				("", ""),
			] {
				assert_eq!(read(text).unwrap().as_str(), prefix, "{text}");
			}
		}

		#[test]
		fn refuses_every_other_wildcard() {
			for text in [
				"/**",
				"*",
				"*/chat",
				"foo/*",
				"foo/*/**",
				"**/foo",
				"foo/**/**",
				"cam-*.hang",
				r"a\/**",
			] {
				assert!(read(text).is_err(), "{text}");
			}
			// The rest is still a prefix, so its slashes are checked.
			for text in ["/foo/**", "foo//**", "foo/", "/foo"] {
				assert!(read(text).is_err(), "{text}");
			}
		}

		#[test]
		fn writes_the_subtree_each_prefix_covers() {
			let prefixes: Prefixes = ["", "foo"].into_iter().map(|text| text.parse().unwrap()).collect();
			assert_eq!(serde_json::to_value(List(prefixes)).unwrap(), serde_json::json!(["**"]));
			let prefixes: Prefixes = ["foo", r"a\/b"].into_iter().map(|text| text.parse().unwrap()).collect();
			assert_eq!(
				serde_json::to_value(List(prefixes)).unwrap(),
				serde_json::json!([r"a\/b/**", "foo/**"])
			);
			// Nothing lies beneath a prefix of the most segments a path holds, and a pattern
			// one segment longer is refused, so it is written as the literal.
			let deepest = vec!["a"; Pattern::MAX_SEGMENTS].join("/");
			let list = List(Prefixes::from(deepest.parse::<Prefix>().unwrap()));
			assert_eq!(serde_json::to_value(&list).unwrap(), serde_json::json!([deepest]));
			let read: List = serde_json::from_value(serde_json::to_value(&list).unwrap()).unwrap();
			assert_eq!(read.0, list.0);
		}
	}
}
