//! Solicited viewer feedback, carried on an echo broadcast.
use crate::snapshot::{Millis, Number, is_zero};
use crate::stats::Transport;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, time::Duration};
/// One cumulative viewer report, keyed by the catalog's rendition IDs.
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

/// Cumulative counters and latest playout measurements for one rendition.
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default, rename_all = "camelCase")]
#[non_exhaustive]
pub struct Rendition {
	/// Cumulative media frames received.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub frames: u64,
	/// Cumulative media payload bytes received.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub bytes: u64,
	/// Cumulative frames decoded.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub decoded: u64,
	/// Cumulative frames received too late for playout.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub late: u64,
	/// Cumulative decode errors.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub errors: u64,
	/// Cumulative playout stalls.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub stalls: u64,
	/// Cumulative audio buffer underruns.
	#[serde_as(as = "Number")]
	#[serde(skip_serializing_if = "is_zero")]
	pub underruns: u64,
	/// Cumulative time spent stalled.
	#[serde_as(as = "Millis")]
	#[serde(skip_serializing_if = "Duration::is_zero")]
	pub stalled: Duration,
	/// The newest media timestamp received and its arrival wall time.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub arrival: Option<Arrival>,
	/// Latest playout latency.
	#[serde_as(as = "Option<Millis>")]
	#[serde(skip_serializing_if = "Option::is_none")]
	pub latency: Option<Duration>,
}

/// The newest received media timestamp paired with its arrival wall time.
#[serde_with::serde_as]
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[non_exhaustive]
pub struct Arrival {
	/// The media timestamp, encoded in milliseconds.
	#[serde_as(as = "Millis")]
	pub timestamp: moq_net::Timestamp,
	/// The arrival wall time, encoded in milliseconds since the MoQ epoch.
	#[serde_as(as = "crate::snapshot::Wall")]
	pub wall: kio::clock::Wall,
}

impl Arrival {
	/// Pair a received media timestamp with the wall time it arrived.
	pub fn new(timestamp: moq_net::Timestamp, wall: kio::clock::Wall) -> Self {
		Self { timestamp, wall }
	}
}

/// The suffix identifying viewer feedback broadcasts at announce time.
pub const SUFFIX: &str = ".echo";
/// The snapshot track carried by an echo broadcast.
pub const TRACK: &str = "feedback.json";
/// Whether a broadcast path identifies viewer feedback rather than playable media.
pub fn is_echo(path: &moq_net::Path) -> bool {
	path.as_str().ends_with(SUFFIX)
}
