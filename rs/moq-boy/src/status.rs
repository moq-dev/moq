//! Status track: JSON state published to viewers each frame.
//!
//! Contains which buttons are held, how many viewers are playing, encoding
//! stats, and an optional location label. Each viewer's latency travels on its
//! own session instead (see [`crate::input`]). Published through
//! [`moq_json`], which skips unchanged values and emits merge-patch
//! deltas between snapshots so frequent small changes stay cheap.

use serde::Serialize;

use crate::emulator::Button;
use crate::stats::StatsReport;

/// A single latency component measurement.
#[derive(Serialize)]
pub struct LatencyEntry {
	pub label: String,
	pub ms: u32,
}

/// Per-frame status broadcast to all viewers on the "status" track.
#[derive(Serialize)]
pub struct Status {
	/// Currently pressed buttons (union across all viewers).
	pub buttons: Vec<Button>,
	/// How many viewers have a session open.
	pub players: usize,
	/// Encoding and emulation performance stats.
	pub stats: StatsReport,
	/// Optional server location label (e.g. "Dallas, TX").
	#[serde(skip_serializing_if = "Option::is_none")]
	pub location: Option<String>,
}

/// Manages the status track, publishing snapshots and deltas via [`moq_json`].
pub struct StatusPublisher {
	producer: moq_json::snapshot::Producer<Status>,
}

impl StatusPublisher {
	pub fn new(broadcast: &mut moq_net::broadcast::Producer) -> anyhow::Result<Self> {
		let producer = broadcast.create_track(
			"status",
			moq_net::track::Info::default()
				.with_timescale(moq_net::Timescale::MILLI)
				// A joiner asks for a fresh snapshot once the deltas outweigh eight of them.
				.with_refresh(8),
		)?;

		Ok(Self {
			producer: moq_json::snapshot::Producer::new(producer, moq_json::snapshot::Config::default()),
		})
	}

	/// Publish status, a no-op if unchanged since the last call.
	///
	/// Also restates the status in a fresh group when a joiner wants one. Status is published every
	/// frame, so checking here answers a joiner within a frame without a task of its own.
	pub fn publish(&mut self, status: &Status) {
		if let std::task::Poll::Ready(Err(err)) = self.producer.poll_refresh(&kio::Waiter::noop()) {
			tracing::warn!(%err, "failed to refresh status");
		}
		if let Err(err) = self
			.producer
			.update(moq_json::Timed::from(status).at(now()))
		{
			tracing::warn!(%err, "failed to publish status");
		}
	}
}

/// Milliseconds on this process's monotonic clock since its origin: the status track's timescale.
fn now() -> moq_net::Timestamp {
	let elapsed = kio::clock::now().saturating_duration_since(kio::clock::origin().0);
	u64::try_from(elapsed.as_millis())
		.ok()
		.and_then(|millis| moq_net::Timestamp::from_millis(millis).ok())
		.expect("a process's uptime fits a timestamp")
}
