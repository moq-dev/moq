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

use crate::{Access, Link};
use moq_pattern::{Pattern, Patterns};
use serde::{Deserialize, Deserializer, Serialize};
use serde_with::{DurationSeconds, serde_as};
use std::collections::BTreeMap;
use std::time::Duration;

/// The wire form of [`Claims`](crate::Claims).
#[serde_with::skip_serializing_none]
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
		let grants = Grants::encode(claims.publish, claims.subscribe);
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
			publish,
			subscribe,
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
		let grants = Grants::encode(Access::new(scope.publish), Access::new(scope.subscribe));
		let paths = |role: Option<Access>| role.map(|role| role.path);
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

/// The grant fields shared by both documents, in whichever encoding they arrived.
struct Grants {
	put: Option<Vec<String>>,
	get: Option<Vec<String>>,
	publish: Option<Access>,
	subscribe: Option<Access>,
}

impl Grants {
	fn encode(publish: Access, subscribe: Access) -> Self {
		// A cap has no prefix form, so one moves the whole document to patterns.
		let prefixes = |role: &Access| -> Option<Vec<String>> {
			if role.rate.is_some() {
				return None;
			}
			role.path
				.iter()
				.map(|pattern| pattern.as_prefix().map(str::to_string))
				.collect()
		};
		let some = |role: Access| (!role.is_unset()).then_some(role);

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
	root: Option<String>,
	#[serde(skip_serializing_if = "BTreeMap::is_empty")]
	mounts: BTreeMap<String, String>,
	#[serde(with = "seconds", skip_serializing_if = "Option::is_none")]
	expires: Option<kio::clock::Wall>,
	#[serde_as(as = "Option<DurationSeconds<u64>>")]
	revalidate: Option<Duration>,
	tier: Option<String>,
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
