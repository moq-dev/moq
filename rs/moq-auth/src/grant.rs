use kio::clock::Wall;
use moq_pattern::Prefixes;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::Access;

/// What a session may do, as the auth server answered.
///
/// A 2xx carrying one of these admits; anything else refuses. A grant that names
/// nothing is a refusal too, and one that asks to be revalidated must say when it
/// expires, so an outage always has a bound the server chose. [`validate`](Self::validate)
/// checks these once at the boundary.
///
/// A field this reader does not know is refused, as an unknown claim is: a field added
/// later may narrow the grant, and reading past it would widen the session. A server
/// tells which fields a relay reads from [`Request::relay`](crate::Request::relay).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(try_from = "crate::wire::Object<crate::wire::Grant>", into = "crate::wire::Grant")]
#[non_exhaustive]
pub struct Grant {
	/// What the session may publish, relative to the root.
	pub publish: Access,

	/// What the session may subscribe to, relative to the root. A cap bounds what it
	/// receives from its subscriptions.
	pub subscribe: Access,

	/// The path the patterns are relative to, replacing the dialed one. This is how a
	/// server aliases a slug to a canonical id. Absent means the dialed path.
	pub root: Option<String>,

	/// Subtrees the session reads from elsewhere: each path, relative to the root,
	/// resolves at the absolute path it maps to. Read-only: nothing is published
	/// beneath one. The patterns still name the path relative to the root.
	pub mounts: BTreeMap<String, String>,

	/// When the session closes, as unix seconds.
	pub expires: Option<Wall>,

	/// How long until the relay asks again, in seconds.
	pub revalidate: Option<Duration>,

	/// The stats tier this session's traffic is bucketed under: `/`-joined segments of
	/// ASCII letters, digits, `.`, `_`, and `-`, as [`tier::validate`](crate::tier::validate) checks.
	pub tier: Option<String>,

	/// The literal path this session's stats record under, independent of its root. Absent records under the empty label.
	pub label: Option<String>,

	/// How the session, another relay, links into the cluster; absent for a client. A
	/// client certificate alone is not a link.
	///
	/// Written beside `peer: true`, which a relay before 0.22 reads in its place, as a plain peer
	/// even for an upstream: refuse a request with no [`Request::relay`](crate::Request::relay)
	/// when that matters. A grant that names a link while its `peer` is false is refused.
	pub link: Option<Link>,
}

/// How a cluster peer (another relay) links to this one.
///
/// Exhaustive on purpose: a new kind of link changes what a relay offers it, so every
/// reader must decide how to treat it rather than fall through to a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Link {
	/// A cluster peer: what it announces entered the cluster elsewhere, not here, and it
	/// discovers hidden routes without asking, for peers that predate the opt-in.
	Peer,
	/// A peer that is upstream: the relay never offers it a route learned from another
	/// upstream peer, so it never carries traffic between two.
	Upstream,
}

impl Link {
	/// The lowercase name the wire carries.
	pub const fn as_str(self) -> &'static str {
		match self {
			Self::Peer => "peer",
			Self::Upstream => "upstream",
		}
	}
}

impl std::str::FromStr for Link {
	type Err = &'static str;
	fn from_str(value: &str) -> Result<Self, Self::Err> {
		match value {
			"peer" => Ok(Self::Peer),
			"upstream" => Ok(Self::Upstream),
			_ => Err("expected peer or upstream"),
		}
	}
}

impl std::fmt::Display for Link {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

impl Serialize for Link {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(self.as_str())
	}
}

impl<'de> Deserialize<'de> for Link {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
		text.parse().map_err(serde::de::Error::custom)
	}
}

impl Grant {
	/// A grant of these patterns, uncapped, with no other restrictions.
	pub fn new(publish: impl Into<Prefixes>, subscribe: impl Into<Prefixes>) -> Self {
		Self {
			publish: Access::new(publish),
			subscribe: Access::new(subscribe),
			..Default::default()
		}
	}

	/// Snapshot the expiry on the ambient clock; one already past is now, and one beyond
	/// the clock's range is never.
	pub fn deadline(&self) -> Option<kio::clock::Instant> {
		// Read the instant first, so a pause between the two reads shortens the grant
		// instead of extending it.
		let start = kio::clock::now();
		let now = kio::clock::wall();
		let remaining = self.expires?.checked_duration_since(now).unwrap_or_default();
		start.checked_add(remaining)
	}

	/// Refuse a grant that admits nothing, asks to be revalidated without a bound or at
	/// no interval, names an invalid stats tier or label, or has already expired.
	pub fn validate(&self) -> crate::Result<()> {
		if self.publish.path.is_empty() && self.subscribe.path.is_empty() {
			return Err(crate::Error::UselessGrant);
		}
		if self.revalidate.is_some() && self.expires.is_none() {
			return Err(crate::Error::UnboundedRevalidate);
		}
		// A zero cadence would have the relay re-check in a tight loop.
		if self.revalidate.is_some_and(|cadence| cadence.is_zero()) {
			return Err(crate::Error::ZeroRevalidate);
		}
		if let Some(tier) = &self.tier {
			crate::tier::validate(tier)?;
		}
		if let Some(label) = &self.label {
			moq_pattern::Pattern::literal(label).map_err(crate::Error::InvalidLabel)?;
		}
		if self.expires.is_some_and(|expires| expires <= kio::clock::wall()) {
			return Err(crate::Error::GrantExpired);
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn prefixes(texts: &[&str]) -> Prefixes {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	fn full() -> Grant {
		Grant {
			publish: Access::new(prefixes(&["alice"])).with_rate(crate::Rate::from_bps(8_000_000).unwrap()),
			subscribe: Access::new(prefixes(&[""])),
			root: Some("pid/room".into()),
			mounts: BTreeMap::new(),
			expires: Some(Wall::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			revalidate: Some(Duration::from_secs(60)),
			tier: Some("websocket".into()),
			label: Some("project/key/123".into()),
			link: Some(Link::Upstream),
		}
	}

	/// A subsecond cadence writes as the nearest whole second, never the zero a relay refuses.
	#[test]
	fn a_subsecond_revalidate_writes_rounded() {
		let mut grant = full();
		for (cadence, seconds) in [(750, 1), (1400, 1), (1600, 2)] {
			grant.revalidate = Some(Duration::from_millis(cadence));
			assert_eq!(serde_json::to_value(&grant).unwrap()["revalidate"], seconds);
		}
	}

	#[test]
	fn round_trips_in_seconds() {
		let grant = full();
		let json = serde_json::to_value(&grant).unwrap();
		assert_eq!(json["expires"], 4_102_444_800_i64);
		assert_eq!(json["revalidate"], 60);
		assert_eq!(
			json["publish"],
			serde_json::json!({"path": ["alice"], "rate": 8_000_000})
		);
		assert_eq!(json["subscribe"], serde_json::json!([""]));
		assert_eq!(json["link"], "upstream");
		assert_eq!(serde_json::from_value::<Grant>(json).unwrap(), grant);
	}

	/// The exact bytes `js/auth/src/interop.test.ts` parses, so both languages read
	/// one wire shape.
	#[test]
	fn serializes_to_the_cross_language_vector() {
		assert_eq!(
			serde_json::to_string(&full()).unwrap(),
			r#"{"publish":{"path":["alice"],"rate":8000000},"subscribe":[""],"root":"pid/room","expires":4102444800,"revalidate":60,"tier":"websocket","label":"project/key/123","link":"upstream","peer":true}"#
		);
	}

	/// A field this reader does not know may narrow the grant, so it refuses the whole
	/// grant rather than widen the session past it.
	#[test]
	fn an_unknown_field_is_refused() {
		for json in [
			r#"{"publish":[""],"publish_rate":8000}"#,
			r#"{"publish":[""],"upstream":true}"#,
			r#"{"publish":[""],"future":{}}"#,
			r#"{"publish":[""],"link":"downstream"}"#,
		] {
			assert!(serde_json::from_str::<Grant>(json).is_err(), "{json}");
		}
	}

	/// TEMPORARY(0.22): `peer` is read as the link a relay before 0.22 knows, and a
	/// grant whose two fields disagree is refused.
	#[test]
	fn peer_is_an_alias_for_a_link() {
		let read = |json: &str| serde_json::from_str::<Grant>(json).map(|grant| grant.link);
		assert_eq!(read(r#"{"publish":[""],"peer":true}"#).unwrap(), Some(Link::Peer));
		assert_eq!(read(r#"{"publish":[""],"peer":false}"#).unwrap(), None);
		assert_eq!(read(r#"{"publish":[""],"link":"peer"}"#).unwrap(), Some(Link::Peer));
		assert_eq!(
			read(r#"{"publish":[""],"link":"upstream","peer":true}"#).unwrap(),
			Some(Link::Upstream)
		);
		assert!(read(r#"{"publish":[""],"link":"upstream","peer":false}"#).is_err());
		assert!(read(r#"{"publish":[""],"link":null}"#).is_err());

		let mut grant = Grant::new(prefixes(&[""]), Prefixes::default());
		grant.link = Some(Link::Peer);
		assert_eq!(
			serde_json::to_string(&grant).unwrap(),
			r#"{"publish":[""],"link":"peer","peer":true}"#
		);
	}

	#[test]
	fn mounts_round_trip_as_an_object() {
		let mut grant = Grant::new(Prefixes::default(), prefixes(&[""]));
		grant.mounts.insert(".svc".into(), ".svc/pid".into());
		let json = serde_json::to_string(&grant).unwrap();
		assert_eq!(json, r#"{"subscribe":[""],"mounts":{".svc":".svc/pid"}}"#);
		assert_eq!(serde_json::from_str::<Grant>(&json).unwrap(), grant);
	}

	#[test]
	fn empty_fields_are_omitted_and_defaulted() {
		let grant = Grant::new(prefixes(&[""]), Prefixes::default());
		assert_eq!(serde_json::to_string(&grant).unwrap(), r#"{"publish":[""]}"#);
		assert_eq!(serde_json::from_str::<Grant>(r#"{"publish":[""]}"#).unwrap(), grant);
	}

	#[kio::test]
	async fn validate_refuses_nothing_unbounded_and_expired() {
		assert!(matches!(Grant::default().validate(), Err(crate::Error::UselessGrant)));
		let mut capped = Grant::default();
		capped.publish.rate = crate::Rate::from_bps(8000).ok();
		assert!(
			matches!(capped.validate(), Err(crate::Error::UselessGrant)),
			"a cap on no paths admits nothing"
		);

		let mut grant = Grant::new(prefixes(&[""]), Prefixes::default());
		grant.validate().unwrap();

		grant.revalidate = Some(Duration::from_secs(1));
		assert!(matches!(grant.validate(), Err(crate::Error::UnboundedRevalidate)));

		// Exact: no grace for an auth server whose clock runs behind.
		let now = kio::clock::wall();
		grant.expires = Some(now);
		assert!(matches!(grant.validate(), Err(crate::Error::GrantExpired)));

		grant.expires = Some(now - Duration::from_secs(1));
		assert!(matches!(grant.validate(), Err(crate::Error::GrantExpired)));

		grant.expires = Some(now + Duration::from_secs(60));
		grant.validate().unwrap();
		kio::clock::sim::advance(Duration::from_secs(60)).await;
		assert!(
			matches!(grant.validate(), Err(crate::Error::GrantExpired)),
			"expiry follows the ambient clock, not the host's"
		);

		grant.expires = Some(now + Duration::from_secs(120));
		grant.revalidate = Some(Duration::ZERO);
		assert!(matches!(grant.validate(), Err(crate::Error::ZeroRevalidate)));
	}

	#[test]
	fn validate_refuses_an_invalid_stats_label() {
		let mut grant = Grant::new(prefixes(&[""]), Prefixes::default());
		grant.label = Some("project/*".into());
		assert!(matches!(grant.validate(), Err(crate::Error::InvalidLabel(_))));
	}

	#[test]
	fn validate_refuses_a_tier_a_metrics_label_would_escape() {
		let mut grant = Grant::new(prefixes(&[""]), Prefixes::default());
		for tier in ["a\"b", "a\\b", "a\nb"] {
			grant.tier = Some(tier.into());
			assert!(
				matches!(grant.validate(), Err(crate::Error::InvalidTier(_))),
				"{tier:?}"
			);
		}
		grant.tier = Some("region/sjc".into());
		grant.validate().unwrap();
	}

	/// The expiry and the instant come from one clock, so time passing before the
	/// snapshot never shortens the grant.
	#[kio::test]
	async fn deadline_is_the_exact_expiry() {
		let start = kio::clock::now();
		let now = kio::clock::wall();
		let mut grant = Grant::new(prefixes(&[""]), Prefixes::default());
		assert_eq!(grant.deadline(), None);

		grant.expires = Some(now + Duration::from_secs(10));
		kio::clock::sim::advance(Duration::from_secs(4)).await;
		assert_eq!(grant.deadline(), Some(start.saturating_add(Duration::from_secs(10))));

		grant.expires = Some(now - Duration::from_secs(1));
		assert_eq!(
			grant.deadline(),
			Some(start.saturating_add(Duration::from_secs(4))),
			"a past expiry is now, not a grace window"
		);
	}

	/// The clock's last time round-trips, and the furthest expiry the wire carries does
	/// not overflow the instant.
	#[kio::test]
	async fn the_last_expiry_fits_the_clock() {
		let mut last = Grant::new(prefixes(&[""]), Prefixes::default());
		last.expires = Some(Wall::UNIX_EPOCH.saturating_add(Duration::MAX));
		let json = serde_json::to_value(&last).unwrap();
		assert_eq!(json["expires"], 18_446_744_073u64);
		let far: Grant = serde_json::from_value(json).unwrap();
		far.validate().unwrap();
		far.deadline();
	}
}
