use moq_pattern::Patterns;
use serde::{Deserialize, Serialize};
use serde_with::{DurationSeconds, TimestampSeconds, serde_as};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

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
	#[serde_as(as = "Option<TimestampSeconds<i64>>")]
	pub expires: Option<SystemTime>,

	/// How long until the relay asks again, in seconds.
	#[serde_as(as = "Option<DurationSeconds<u64>>")]
	pub revalidate: Option<Duration>,

	/// An opaque label handed to stats, so traffic can be bucketed.
	pub tier: Option<String>,

	/// The session is a cluster peer (another relay): what it announces entered
	/// the cluster elsewhere, not here.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub peer: bool,

	/// The cluster peer is upstream: the relay never offers it a route learned
	/// from another upstream peer, so it never carries traffic between two.
	/// Requires `peer`.
	#[serde(skip_serializing_if = "std::ops::Not::not")]
	pub upstream: bool,
}

impl Grant {
	/// A grant of these patterns and nothing else.
	pub fn new(publish: Patterns, subscribe: Patterns) -> Self {
		Self {
			publish,
			subscribe,
			..Default::default()
		}
	}

	/// Snapshot the expiry on Tokio's clock; one already past is now, and one beyond
	/// the clock's range is never.
	#[cfg(feature = "tokio")]
	pub fn deadline(&self) -> Option<tokio::time::Instant> {
		let remaining = self.expires?.duration_since(SystemTime::now()).unwrap_or_default();
		tokio::time::Instant::now().checked_add(remaining)
	}

	/// Refuse a grant that admits nothing, marks a non-peer upstream, asks to be
	/// revalidated without a bound or at no interval, or has already expired.
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
		if self.expires.is_some_and(|expires| expires <= SystemTime::now()) {
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
			expires: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			revalidate: Some(Duration::from_secs(60)),
			tier: Some("websocket".into()),
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
			expires: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			revalidate: Some(Duration::from_secs(60)),
			tier: Some("websocket".into()),
			peer: true,
			upstream: true,
		};
		assert_eq!(
			serde_json::to_string(&grant).unwrap(),
			r#"{"publish":["alice/**"],"subscribe":["**"],"root":"pid/room","expires":4102444800,"revalidate":60,"tier":"websocket","peer":true,"upstream":true}"#
		);
	}

	#[test]
	fn mounts_round_trip_as_an_object() {
		let mut grant = Grant::new(Patterns::new(), patterns(&["**"]));
		grant.mounts.insert(".svc".into(), ".svc/pid".into());
		let json = serde_json::to_string(&grant).unwrap();
		assert_eq!(json, r#"{"subscribe":["**"],"mounts":{".svc":".svc/pid"}}"#);
		assert_eq!(serde_json::from_str::<Grant>(&json).unwrap(), grant);
	}

	#[test]
	fn empty_fields_are_omitted_and_defaulted() {
		let grant = Grant::new(patterns(&["**"]), Patterns::new());
		assert_eq!(serde_json::to_string(&grant).unwrap(), r#"{"publish":["**"]}"#);
		assert_eq!(serde_json::from_str::<Grant>(r#"{"publish":["**"]}"#).unwrap(), grant);
	}

	#[test]
	fn validate_refuses_nothing_unbounded_and_expired() {
		assert!(matches!(Grant::default().validate(), Err(crate::Error::UselessGrant)));

		let mut grant = Grant::new(patterns(&["**"]), Patterns::new());
		grant.validate().unwrap();

		grant.revalidate = Some(Duration::from_secs(1));
		assert!(matches!(grant.validate(), Err(crate::Error::UnboundedRevalidate)));

		// Exact: no grace for an auth server whose clock runs behind.
		grant.expires = Some(SystemTime::now());
		assert!(matches!(grant.validate(), Err(crate::Error::GrantExpired)));

		grant.expires = Some(SystemTime::now() - Duration::from_secs(1));
		assert!(matches!(grant.validate(), Err(crate::Error::GrantExpired)));

		grant.expires = Some(SystemTime::now() + Duration::from_secs(60));
		grant.validate().unwrap();

		grant.revalidate = Some(Duration::ZERO);
		assert!(matches!(grant.validate(), Err(crate::Error::ZeroRevalidate)));
	}

	#[test]
	fn validate_refuses_an_upstream_that_is_not_a_peer() {
		let mut grant = Grant::new(patterns(&["**"]), Patterns::new());
		grant.upstream = true;
		assert!(matches!(grant.validate(), Err(crate::Error::UpstreamWithoutPeer)));
		grant.peer = true;
		grant.validate().unwrap();
	}

	#[cfg(feature = "tokio")]
	#[tokio::test(start_paused = true)]
	async fn deadline_is_the_exact_expiry() {
		let start = tokio::time::Instant::now();
		let mut grant = Grant::new(patterns(&["**"]), Patterns::new());
		assert_eq!(grant.deadline(), None);

		grant.expires = Some(SystemTime::now() + Duration::from_secs(10));
		let deadline = grant.deadline().unwrap();
		assert!(deadline <= start + Duration::from_secs(10), "not later than the expiry");
		assert!(deadline > start + Duration::from_secs(9));

		grant.expires = Some(SystemTime::now() - Duration::from_secs(1));
		assert_eq!(
			grant.deadline(),
			Some(start),
			"a past expiry is now, not a grace window"
		);
	}

	/// Regression: the furthest `expires` the wire carries overflowed `Instant` on
	/// clocks with a narrower range than `SystemTime` (macOS) and panicked. Windows'
	/// `SystemTime` cannot represent it, so the grant does not parse there.
	#[cfg(all(feature = "tokio", unix))]
	#[tokio::test(start_paused = true)]
	async fn deadline_past_the_clock_is_never() {
		let start = tokio::time::Instant::now();
		let grant: Grant = serde_json::from_str(r#"{"publish":["**"],"expires":9223372036854775807}"#).unwrap();
		// Linux's clock reaches that far; macOS's does not, which reads as no expiry.
		let deadline = grant.deadline();
		assert!(deadline.is_none_or(|at| at > start + Duration::from_secs(1 << 40)));
	}
}
