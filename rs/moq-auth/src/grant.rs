use kio::clock::Wall;
use moq_pattern::Patterns;
use serde::{Deserialize, Serialize};
use serde_with::{DurationSeconds, serde_as};
use std::collections::BTreeMap;
use std::time::Duration;

/// What a session may do, as the auth server answered.
///
/// A 2xx carrying one of these admits; anything else refuses. A grant that names
/// nothing is a refusal too, and one that asks to be revalidated must say when it
/// expires, so an outage always has a bound the server chose. [`validate`](Self::validate)
/// checks these once at the boundary.
#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
#[non_exhaustive]
pub struct Grant {
	/// Patterns the session may publish, relative to the root.
	#[serde(skip_serializing_if = "Patterns::is_empty")]
	pub publish: Patterns,

	/// Patterns the session may subscribe to, relative to the root.
	#[serde(skip_serializing_if = "Patterns::is_empty")]
	pub subscribe: Patterns,

	/// The path the patterns are relative to, replacing the dialed one. This is how a
	/// server aliases a slug to a canonical id. Absent means the dialed path.
	pub root: Option<String>,

	/// Subtrees the session reads from elsewhere: each path, relative to the root,
	/// resolves at the absolute path it maps to. Read-only: nothing is published
	/// beneath one. The patterns still name the path relative to the root.
	#[serde(skip_serializing_if = "BTreeMap::is_empty")]
	pub mounts: BTreeMap<String, String>,

	/// When the session closes, as unix seconds.
	#[serde_as(as = "Option<kio::clock::serde::UnixSeconds>")]
	pub expires: Option<Wall>,

	/// How long until the relay asks again, in seconds.
	#[serde_as(as = "Option<DurationSeconds<u64>>")]
	pub revalidate: Option<Duration>,

	/// The stats tier this session's traffic is bucketed under: `/`-joined segments of
	/// ASCII letters, digits, `.`, `_`, and `-`, as [`tier::validate`](crate::tier::validate) checks.
	pub tier: Option<String>,

	/// The literal path this session's stats record under, independent of its root. Absent records under the empty label.
	pub label: Option<String>,

	/// The session is a cluster peer (another relay): what it announces entered
	/// the cluster elsewhere, not here, and it discovers hidden routes without
	/// asking, for peers that predate the opt-in. A client certificate alone is not.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub peer: bool,

	/// The cluster peer is upstream: the relay never offers it a route learned
	/// from another upstream peer, so it never carries traffic between two.
	/// Requires `peer`.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub upstream: bool,
}

impl Grant {
	/// A grant of these patterns, with no other restrictions.
	pub fn new(publish: impl Into<Patterns>, subscribe: impl Into<Patterns>) -> Self {
		Self {
			publish: publish.into(),
			subscribe: subscribe.into(),
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

	/// Refuse a grant that admits nothing, marks a non-peer upstream, asks to be
	/// revalidated without a bound or at no interval, names an invalid stats tier or
	/// label, or has already expired.
	pub fn validate(&self) -> crate::Result<()> {
		if self.publish.is_empty() && self.subscribe.is_empty() {
			return Err(crate::Error::UselessGrant);
		}
		if self.upstream && !self.peer {
			return Err(crate::Error::UpstreamWithoutPeer);
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

	fn patterns(texts: &[&str]) -> Patterns {
		texts.iter().map(|text| text.parse().unwrap()).collect()
	}

	#[test]
	fn round_trips_in_seconds() {
		let grant = Grant {
			publish: patterns(&["alice/**"]),
			subscribe: patterns(&["**"]),
			root: Some("pid/room".into()),
			mounts: BTreeMap::new(),
			expires: Some(Wall::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			revalidate: Some(Duration::from_secs(60)),
			tier: Some("websocket".into()),
			label: Some("project/key/123".into()),
			peer: true,
			upstream: true,
		};
		let json = serde_json::to_value(&grant).unwrap();
		assert_eq!(json["expires"], 4_102_444_800_i64);
		assert_eq!(json["revalidate"], 60);
		assert_eq!(json["publish"], serde_json::json!(["alice/**"]));
		assert_eq!(json["peer"], true);
		assert_eq!(json["upstream"], true);
		assert_eq!(serde_json::from_value::<Grant>(json).unwrap(), grant);
	}

	/// The exact bytes `js/auth/src/interop.test.ts` parses, so both languages read
	/// one wire shape.
	#[test]
	fn serializes_to_the_cross_language_vector() {
		let grant = Grant {
			publish: patterns(&["alice/**"]),
			subscribe: patterns(&["**"]),
			root: Some("pid/room".into()),
			mounts: BTreeMap::new(),
			expires: Some(Wall::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			revalidate: Some(Duration::from_secs(60)),
			tier: Some("websocket".into()),
			label: Some("project/key/123".into()),
			peer: true,
			upstream: true,
		};
		assert_eq!(
			serde_json::to_string(&grant).unwrap(),
			r#"{"publish":["alice/**"],"subscribe":["**"],"root":"pid/room","expires":4102444800,"revalidate":60,"tier":"websocket","label":"project/key/123","peer":true,"upstream":true}"#
		);
	}

	#[test]
	fn mounts_round_trip_as_an_object() {
		let mut grant = Grant::new(Patterns::default(), patterns(&["**"]));
		grant.mounts.insert(".svc".into(), ".svc/pid".into());
		let json = serde_json::to_string(&grant).unwrap();
		assert_eq!(json, r#"{"subscribe":["**"],"mounts":{".svc":".svc/pid"}}"#);
		assert_eq!(serde_json::from_str::<Grant>(&json).unwrap(), grant);
	}

	#[test]
	fn empty_fields_are_omitted_and_defaulted() {
		let grant = Grant::new(patterns(&["**"]), Patterns::default());
		assert_eq!(serde_json::to_string(&grant).unwrap(), r#"{"publish":["**"]}"#);
		assert_eq!(serde_json::from_str::<Grant>(r#"{"publish":["**"]}"#).unwrap(), grant);
	}

	#[kio::test]
	async fn validate_refuses_nothing_unbounded_and_expired() {
		assert!(matches!(Grant::default().validate(), Err(crate::Error::UselessGrant)));

		let mut grant = Grant::new(patterns(&["**"]), Patterns::default());
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
	fn validate_refuses_an_upstream_that_is_not_a_peer() {
		let mut grant = Grant::new(patterns(&["**"]), Patterns::default());
		grant.upstream = true;
		assert!(matches!(grant.validate(), Err(crate::Error::UpstreamWithoutPeer)));
		grant.peer = true;
		grant.validate().unwrap();
	}

	#[test]
	fn validate_refuses_an_invalid_stats_label() {
		let mut grant = Grant::new(patterns(&["**"]), Patterns::default());
		grant.label = Some("project/*".into());
		assert!(matches!(grant.validate(), Err(crate::Error::InvalidLabel(_))));
	}

	#[test]
	fn validate_refuses_a_tier_a_metrics_label_would_escape() {
		let mut grant = Grant::new(patterns(&["**"]), Patterns::default());
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
		let mut grant = Grant::new(patterns(&["**"]), Patterns::default());
		assert_eq!(grant.deadline(), None);

		grant.expires = Some(now + Duration::from_secs(10));
		kio::clock::sim::advance(Duration::from_secs(4)).await;
		assert_eq!(grant.deadline(), Some(start + Duration::from_secs(10)));

		grant.expires = Some(now - Duration::from_secs(1));
		assert_eq!(
			grant.deadline(),
			Some(start + Duration::from_secs(4)),
			"a past expiry is now, not a grace window"
		);
	}

	#[kio::test]
	async fn expiry_saturates_to_the_wall_range() {
		let parse = |expires: i64| {
			serde_json::from_value::<Grant>(serde_json::json!({"publish":["**"],"expires":expires})).unwrap()
		};
		let past = parse(-1);
		assert_eq!(past.expires, Some(Wall::UNIX_EPOCH));
		assert!(matches!(past.validate(), Err(crate::Error::GrantExpired)));

		// The furthest expiry the wire carries must not overflow the instant.
		let far = parse(i64::MAX);
		assert_eq!(far.expires, Some(Wall::UNIX_EPOCH.saturating_add(Duration::MAX)));
		far.validate().unwrap();
		far.deadline();
	}
}
