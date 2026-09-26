use moq_pattern::Patterns;
use serde::{Deserialize, Serialize};
use serde_with::{DurationSeconds, TimestampSeconds, serde_as};
use std::time::{Duration, SystemTime};

/// A grant that expired this recently still stands: the auth server's clock may run behind.
pub(crate) const CLOCK_SKEW: Duration = Duration::from_secs(5);

/// How long until `at`. A deadline up to [`CLOCK_SKEW`] in the past still has the
/// remaining window; anything older is zero. Future deadlines are unchanged, so a
/// grant that expires in ten seconds still expires in ten seconds.
fn until(at: SystemTime) -> Duration {
	match at.duration_since(SystemTime::now()) {
		Ok(remaining) => remaining,
		Err(late) => CLOCK_SKEW.saturating_sub(late.duration()),
	}
}

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

	/// Snapshot the expiry on Tokio's clock, allowing five seconds of past clock skew.
	#[cfg(feature = "tokio")]
	pub fn deadline(&self) -> Option<tokio::time::Instant> {
		self.expires.map(|at| tokio::time::Instant::now() + until(at))
	}

	/// Refuse a grant that admits nothing, asks to be revalidated without a bound or
	/// at no interval, or has already expired. A few seconds of clock skew are
	/// tolerated so an auth server whose clock runs behind still admits.
	pub fn validate(&self) -> crate::Result<()> {
		if self.publish.is_empty() && self.subscribe.is_empty() {
			return Err(crate::Error::UselessGrant);
		}
		if self.revalidate.is_some() && self.expires.is_none() {
			return Err(crate::Error::UnboundedRevalidate);
		}
		// A zero cadence would have the relay re-check in a tight loop.
		if self.revalidate.is_some_and(|cadence| cadence.is_zero()) {
			return Err(crate::Error::ZeroRevalidate);
		}
		if self.expires.is_some_and(|expires| until(expires).is_zero()) {
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
			expires: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			revalidate: Some(Duration::from_secs(60)),
			tier: Some("websocket".into()),
			peer: true,
		};
		let json = serde_json::to_value(&grant).unwrap();
		assert_eq!(json["expires"], 4_102_444_800_i64);
		assert_eq!(json["revalidate"], 60);
		assert_eq!(json["publish"], serde_json::json!(["alice/**"]));
		assert_eq!(json["peer"], true);
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
			expires: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(4_102_444_800)),
			revalidate: Some(Duration::from_secs(60)),
			tier: Some("websocket".into()),
			peer: true,
		};
		assert_eq!(
			serde_json::to_string(&grant).unwrap(),
			r#"{"publish":["alice/**"],"subscribe":["**"],"root":"pid/room","expires":4102444800,"revalidate":60,"tier":"websocket","peer":true}"#
		);
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

		grant.expires = Some(SystemTime::now() - Duration::from_secs(1));
		grant.validate().unwrap();

		grant.expires = Some(SystemTime::now() - CLOCK_SKEW - Duration::from_secs(1));
		assert!(matches!(grant.validate(), Err(crate::Error::GrantExpired)));

		grant.expires = Some(SystemTime::now() + Duration::from_secs(60));
		grant.validate().unwrap();

		grant.revalidate = Some(Duration::ZERO);
		assert!(matches!(grant.validate(), Err(crate::Error::ZeroRevalidate)));
	}
}
