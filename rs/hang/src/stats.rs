//! Publisher media and transport snapshots.
use crate::snapshot::{Millis, Number, is_zero};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};
/// One cumulative publisher report, keyed by the catalog's rendition IDs.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[serde(bound(serialize = "E: Serialize", deserialize = "E: serde::de::DeserializeOwned + Default"))]
#[non_exhaustive]
pub struct Snapshot<E = ()> {
	/// The reporting connection's transport measurements.
	#[serde(skip_serializing_if = "Transport::is_empty")]
	pub transport: Transport,
	/// Per-rendition counters, including entries with all counters reset to zero.
	#[serde(skip_serializing_if = "BTreeMap::is_empty")]
	pub renditions: BTreeMap<String, Rendition>,
	/// Application-defined root sections flattened beside the media report.
	#[serde(flatten)]
	pub ext: E,
}

/// Cumulative counters for one locally published rendition.
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Rendition {
	/// Cumulative frames sent.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub frames: u64,
	/// Cumulative media payload bytes sent.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub bytes: u64,
	/// Cumulative keyframes sent.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub keyframes: u64,
	/// Cumulative frames skipped before sending.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub skipped: u64,
	/// Latest target encoder bitrate, in bits per second.
	#[serde_as(as = "Option<Number>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub bitrate: Option<u64>,
}
/// Transport gauges and cumulative loss counters known to the observer.
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Transport {
	/// Latest round-trip time.
	#[serde_as(as = "Option<Millis>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub rtt: Option<Duration>,
	/// Latest estimated connection bitrate, in bits per second.
	#[serde_as(as = "Option<Number>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub rate: Option<u64>,
	/// Cumulative bytes reported lost by the transport.
	#[serde_as(as = "Option<Number>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub lost_bytes: Option<u64>,
	/// Cumulative packets reported lost by the transport.
	#[serde_as(as = "Option<Number>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub lost_packets: Option<u64>,
	/// Age of the transport sample when the snapshot was taken.
	#[serde_as(as = "Option<Millis>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub age: Option<Duration>,
}
impl Transport {
	/// Whether the observer has no transport measurements to encode.
	pub(crate) fn is_empty(&self) -> bool {
		self.rtt.is_none()
			&& self.rate.is_none()
			&& self.lost_bytes.is_none()
			&& self.lost_packets.is_none()
			&& self.age.is_none()
	}
}
