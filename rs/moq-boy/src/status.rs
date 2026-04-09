//! Status track: JSON state published to viewers each frame.
//!
//! Contains which buttons are held, per-viewer latency measurements,
//! encoding stats, and an optional location label. Only published
//! when the JSON changes from the previous frame to avoid waste.

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

/// Manages the status track, only publishing when content changes.
pub struct StatusPublisher {
	producer: moq_lite::TrackProducer,
	last_json: String,
}

impl StatusPublisher {
	pub fn new(broadcast: &mut moq_lite::BroadcastProducer) -> anyhow::Result<Self> {
		let track = moq_lite::Track {
			name: "status".to_string(),
			priority: 10,
		};
		let producer = broadcast.create_track(track)?;

		Ok(Self {
			producer,
			last_json: String::new(),
		})
	}

	/// Publish status if it changed since last call.
	pub fn publish(&mut self, status: &Status) {
		let json = serde_json::to_string(status).unwrap();
		if json != self.last_json {
			self.last_json = json.clone();
			if let Ok(mut group) = self.producer.append_group() {
				let _ = group.write_frame(json.into_bytes());
				let _ = group.finish();
			}
		}
	}
}
