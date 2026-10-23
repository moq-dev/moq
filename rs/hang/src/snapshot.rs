//! Shared JSON adapters for media snapshots.
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeAs, SerializeAs};
use std::time::Duration;

pub(crate) fn is_zero(value: &u64) -> bool {
	*value == 0
}
pub(crate) struct Number;
impl SerializeAs<u64> for Number {
	fn serialize_as<S: serde::Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
		if *value > crate::catalog::MAX_SAFE_INTEGER {
			return Err(serde::ser::Error::custom("snapshot integer exceeds JSON safe range"));
		}
		value.serialize(serializer)
	}
}
impl<'de> DeserializeAs<'de, u64> for Number {
	fn deserialize_as<D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
		let value = u64::deserialize(deserializer)?;
		if value > crate::catalog::MAX_SAFE_INTEGER {
			return Err(serde::de::Error::custom("snapshot integer exceeds JSON safe range"));
		}
		Ok(value)
	}
}

pub(crate) struct Millis;
impl SerializeAs<Duration> for Millis {
	fn serialize_as<S: serde::Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
		let value = u64::try_from(value.as_millis()).map_err(serde::ser::Error::custom)?;
		Number::serialize_as(&value, serializer)
	}
}
impl<'de> DeserializeAs<'de, Duration> for Millis {
	fn deserialize_as<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
		Number::deserialize_as(deserializer).map(Duration::from_millis)
	}
}
impl SerializeAs<moq_net::Timestamp> for Millis {
	fn serialize_as<S: serde::Serializer>(value: &moq_net::Timestamp, serializer: S) -> Result<S::Ok, S::Error> {
		let value = u64::try_from(value.as_millis()).map_err(serde::ser::Error::custom)?;
		Number::serialize_as(&value, serializer)
	}
}
impl<'de> DeserializeAs<'de, moq_net::Timestamp> for Millis {
	fn deserialize_as<D: serde::Deserializer<'de>>(deserializer: D) -> Result<moq_net::Timestamp, D::Error> {
		moq_net::Timestamp::from_millis(Number::deserialize_as(deserializer)?).map_err(serde::de::Error::custom)
	}
}

pub(crate) struct Wall;
fn epoch() -> std::time::SystemTime {
	std::time::UNIX_EPOCH + Duration::from_millis(crate::catalog::MOQ_EPOCH_UNIX_MILLIS)
}
impl SerializeAs<std::time::SystemTime> for Wall {
	fn serialize_as<S: serde::Serializer>(value: &std::time::SystemTime, serializer: S) -> Result<S::Ok, S::Error> {
		let duration = value.duration_since(epoch()).map_err(serde::ser::Error::custom)?;
		Millis::serialize_as(&duration, serializer)
	}
}
impl<'de> DeserializeAs<'de, std::time::SystemTime> for Wall {
	fn deserialize_as<D: serde::Deserializer<'de>>(deserializer: D) -> Result<std::time::SystemTime, D::Error> {
		epoch()
			.checked_add(Millis::deserialize_as(deserializer)?)
			.ok_or_else(|| serde::de::Error::custom("snapshot wall clock overflow"))
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::{Catalog, echo, stats};

	fn roundtrip<T>(input: &str)
	where
		T: serde::de::DeserializeOwned + Serialize,
	{
		let value: serde_json::Value = serde_json::from_str(input).unwrap();
		let decoded: T = serde_json::from_value(value.clone()).unwrap();
		assert_eq!(serde_json::to_value(decoded).unwrap(), value);
	}

	#[test]
	fn shared_snapshots_roundtrip() {
		roundtrip::<stats::Snapshot>(include_str!("../fixtures/stats-snapshot.json"));
		roundtrip::<echo::Snapshot>(include_str!("../fixtures/echo-snapshot.json"));
		roundtrip::<Catalog>(include_str!("../fixtures/catalog-stats-echo.json"));
		roundtrip::<stats::Snapshot>(include_str!("../fixtures/stats-zero.json"));
		roundtrip::<echo::Snapshot>(include_str!("../fixtures/echo-zero.json"));
	}

	#[test]
	fn defaults_and_unknown_fields() {
		assert_eq!(
			serde_json::to_value(stats::Snapshot::<()>::default()).unwrap(),
			serde_json::json!({})
		);
		assert_eq!(
			serde_json::to_value(echo::Snapshot::<()>::default()).unwrap(),
			serde_json::json!({})
		);
		let rendition: stats::Rendition = serde_json::from_str(r#"{"future":17}"#).unwrap();
		assert_eq!(rendition, stats::Rendition::default());
		assert_eq!(serde_json::to_value(rendition).unwrap(), serde_json::json!({}));
		let catalog = Catalog::<()>::from_str("{}").unwrap();
		assert!(catalog.stats.is_none() && catalog.echo.is_none());
		assert_eq!(catalog.to_json().unwrap(), "{}");
	}

	#[test]
	fn missing_counters_mean_zero_but_unknown_measurements_stay_absent() {
		let stats: stats::Snapshot = serde_json::from_str(r#"{"renditions":{"reset":{}}}"#).unwrap();
		assert_eq!(stats.renditions["reset"].frames, 0);
		assert_eq!(stats.renditions["reset"].bitrate, None);
		assert_eq!(stats.transport.rtt, None);
		assert_eq!(stats.transport.lost_packets, None);
		let echo: echo::Snapshot = serde_json::from_str(r#"{"renditions":{"reset":{}}}"#).unwrap();
		assert_eq!(echo.renditions["reset"].stalled, Duration::ZERO);
		assert_eq!(echo.renditions["reset"].latency, None);
	}

	#[test]
	fn optional_null_measurements_are_unavailable() {
		roundtrip::<stats::Snapshot>(r#"{"renditions":{"hd":{}}}"#);
		let stats: stats::Snapshot =
			serde_json::from_str(r#"{"transport":{"rtt":null},"renditions":{"hd":{"bitrate":null}}}"#).unwrap();
		assert_eq!(stats.transport.rtt, None);
		assert_eq!(stats.renditions["hd"].bitrate, None);
		let echo: echo::Snapshot =
			serde_json::from_str(r#"{"renditions":{"hd":{"latency":null,"arrival":null}}}"#).unwrap();
		assert_eq!(echo.renditions["hd"].latency, None);
		assert_eq!(echo.renditions["hd"].arrival, None);
	}

	#[test]
	fn extensions_roundtrip_at_both_roles() {
		#[derive(Serialize, Deserialize, Debug, Default, PartialEq)]
		struct Ext {
			mpegts: serde_json::Value,
		}
		let input = r#"{"mpegts":{"packets":42}}"#;
		roundtrip::<stats::Snapshot<Ext>>(input);
		roundtrip::<echo::Snapshot<Ext>>(input);
		let base: stats::Snapshot = serde_json::from_str(input).unwrap();
		assert!(base.renditions.is_empty());
	}

	#[test]
	fn malformed_counters_refuse_instead_of_rounding() {
		let cases: Vec<serde_json::Value> =
			serde_json::from_str(include_str!("../fixtures/snapshot-invalid.json")).unwrap();
		for case in cases {
			assert!(
				serde_json::from_value::<stats::Rendition>(case.clone()).is_err(),
				"{case}"
			);
		}
		for input in [
			r#"{"stalled":-1}"#,
			r#"{"latency":0.5}"#,
			r#"{"arrival":{"timestamp":9007199254740992,"wall":0}}"#,
			r#"{"arrival":{"timestamp":0,"wall":-1}}"#,
			r#"{"arrival":{}}"#,
			r#"{"arrival":{"timestamp":0}}"#,
			r#"{"arrival":{"wall":0}}"#,
		] {
			assert!(serde_json::from_str::<echo::Rendition>(input).is_err(), "{input}");
		}
		let rendition = stats::Rendition {
			frames: crate::catalog::MAX_SAFE_INTEGER + 1,
			..Default::default()
		};
		assert!(serde_json::to_value(rendition).is_err());
	}

	#[test]
	fn echo_suffix_filters_only_the_broadcast_name() {
		assert!(echo::is_echo(&moq_net::Path::new("room/alice.echo")));
		assert!(!echo::is_echo(&moq_net::Path::new("room.echo/alice")));
		assert!(!echo::is_echo(&moq_net::Path::new("room/alice.echoes")));
	}

	#[test]
	fn media_and_wall_units_are_milliseconds() {
		let report: echo::Snapshot = serde_json::from_str(include_str!("../fixtures/echo-snapshot.json")).unwrap();
		let arrival = report.renditions["hd"].arrival.as_ref().unwrap();
		assert_eq!(arrival.timestamp, moq_net::Timestamp::from_millis(30_000).unwrap());
		assert_eq!(
			arrival.wall.duration_since(epoch()).unwrap(),
			Duration::from_millis(175_184_640_000)
		);
		assert_eq!(report.renditions["hd"].stalled, Duration::from_millis(120));
		assert_eq!(report.transport.rtt, Some(Duration::from_millis(24)));
	}
}
