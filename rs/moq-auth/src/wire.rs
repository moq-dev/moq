//! The JSON encoding of the grants in [`Claims`](crate::Claims) and [`Scope`](crate::Scope).
//!
//! Grants were once prefix lists named `put` and `get`, and every published `moq-token`
//! reader still expects them. A prefix `p` means exactly the pattern `p/**` (and `""`
//! means `**`), so grants that are all subtrees are written the old way and every
//! reader, old or new, agrees on what they grant. Anything a prefix can't say is written
//! as `publish` and `subscribe`, which an old reader sees as granting nothing and
//! refuses. Both encodings are read; one document never mixes them.

use moq_pattern::{Pattern, Patterns};
use serde::{Deserialize, Serialize};
use serde_with::{OneOrMany, TimestampSeconds, formats::PreferMany, serde_as};

/// The wire form of [`Claims`](crate::Claims).
#[serde_with::skip_serializing_none]
#[serde_as]
#[derive(Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Claims {
	#[serde(skip_serializing_if = "String::is_empty")]
	root: String,
	#[serde_as(as = "Option<OneOrMany<_, PreferMany>>")]
	put: Option<Vec<String>>,
	#[serde_as(as = "Option<OneOrMany<_, PreferMany>>")]
	get: Option<Vec<String>>,
	publish: Option<Patterns>,
	subscribe: Option<Patterns>,
	#[serde_as(as = "Option<TimestampSeconds<i64>>")]
	exp: Option<std::time::SystemTime>,
	#[serde_as(as = "Option<TimestampSeconds<i64>>")]
	iat: Option<std::time::SystemTime>,
}

impl From<crate::Claims> for Claims {
	fn from(claims: crate::Claims) -> Self {
		let grants = Grants::encode(claims.publish, claims.subscribe);
		Self {
			root: claims.root,
			put: grants.put,
			get: grants.get,
			publish: grants.publish,
			subscribe: grants.subscribe,
			exp: claims.expires,
			iat: claims.issued,
		}
	}
}

impl TryFrom<Claims> for crate::Claims {
	type Error = String;

	fn try_from(wire: Claims) -> Result<Self, Self::Error> {
		let grants = Grants {
			put: wire.put,
			get: wire.get,
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
	put: Option<Vec<String>>,
	get: Option<Vec<String>>,
	publish: Option<Patterns>,
	subscribe: Option<Patterns>,
}

impl From<crate::Scope> for Scope {
	fn from(scope: crate::Scope) -> Self {
		let grants = Grants::encode(scope.publish, scope.subscribe);
		Self {
			root: scope.root,
			put: grants.put,
			get: grants.get,
			publish: grants.publish,
			subscribe: grants.subscribe,
		}
	}
}

impl TryFrom<Scope> for crate::Scope {
	type Error = String;

	fn try_from(wire: Scope) -> Result<Self, Self::Error> {
		let grants = Grants {
			put: wire.put,
			get: wire.get,
			publish: wire.publish,
			subscribe: wire.subscribe,
		};
		let (publish, subscribe) = grants.decode()?;
		Ok(Self {
			root: wire.root,
			publish,
			subscribe,
		})
	}
}

/// The grant fields shared by both documents, in whichever encoding they arrived.
struct Grants {
	put: Option<Vec<String>>,
	get: Option<Vec<String>>,
	publish: Option<Patterns>,
	subscribe: Option<Patterns>,
}

impl Grants {
	fn encode(publish: Patterns, subscribe: Patterns) -> Self {
		let prefixes = |patterns: &Patterns| -> Option<Vec<String>> {
			patterns
				.iter()
				.map(|pattern| pattern.as_prefix().map(str::to_string))
				.collect()
		};
		let some = |patterns: Patterns| (!patterns.is_empty()).then_some(patterns);

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

	fn decode(self) -> Result<(Patterns, Patterns), String> {
		let legacy = self.put.is_some() || self.get.is_some();
		if !legacy {
			return Ok((self.publish.unwrap_or_default(), self.subscribe.unwrap_or_default()));
		}
		if self.publish.is_some() || self.subscribe.is_some() {
			return Err("mixes the legacy put/get fields with publish/subscribe".to_string());
		}

		let subtrees = |prefixes: Option<Vec<String>>| -> Result<Patterns, String> {
			prefixes
				.unwrap_or_default()
				.iter()
				.map(|prefix| Pattern::subtree(prefix).map_err(|err| format!("legacy prefix {prefix:?}: {err}")))
				.collect()
		};
		Ok((subtrees(self.put)?, subtrees(self.get)?))
	}
}
