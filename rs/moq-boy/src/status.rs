//! Status track: JSON state published to viewers each frame.
//!
//! Contains which buttons are held, per-viewer latency measurements,
//! encoding stats, and an optional location label. Published through
//! [`moq_json`], which skips unchanged values and emits merge-patch
//! deltas between snapshots so frequent small changes stay cheap.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::emulator::Button;
use crate::stats::StatsReport;

/// A single latency component measurement.
#[derive(Serialize, Clone)]
pub struct LatencyEntry {
	pub label: String,
	pub ms: u32,
}

/// Per-frame status broadcast to all viewers on the "status" track.
#[derive(Serialize)]
pub struct Status {
	/// Currently pressed buttons (union across all viewers).
	pub buttons: Vec<Button>,
	/// Per-viewer latency breakdown: viewer_id → ordered list of components.
	pub latency: BTreeMap<String, Vec<LatencyEntry>>,
	/// Encoding and emulation performance stats.
	pub stats: StatsReport,
	/// Optional server location label (e.g. "Dallas, TX").
	#[serde(skip_serializing_if = "Option::is_none")]
	pub location: Option<String>,
}

/// Manages the status track, publishing snapshots and deltas via [`moq_json`].
pub struct StatusPublisher {
	producer: moq_json::Producer<Status>,
}

impl StatusPublisher {
	pub fn new(broadcast: &mut moq_net::BroadcastProducer) -> anyhow::Result<Self> {
		let track = moq_net::Track {
			name: "status".to_string(),
			priority: 10,
		};
		let producer = broadcast.create_track(track)?;

		Ok(Self {
			producer: moq_json::Producer::new(producer, moq_json::Config::default()),
		})
	}

	/// Publish status, a no-op if unchanged since the last call.
	pub fn publish(&mut self, status: &Status) {
		if let Err(err) = self.producer.update(status) {
			tracing::warn!(%err, "failed to publish status");
		}
	}
}
