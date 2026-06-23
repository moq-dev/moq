use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Shared payload counters for one broadcast slot: three atomics the model bumps
/// as media flows.
///
/// The model carries an `Arc<Usage>` on its producer/consumer types (attached at
/// the broadcast via [`crate::BroadcastProducer::with_meter`] /
/// [`crate::BroadcastConsumer::with_meter`] and inherited by every track, group,
/// and frame underneath) and bumps it directly, with no `Option` check or
/// dispatch: a bump is a single `fetch_add`. The stats layer owns one per
/// `(broadcast, tier, role)` inside the same `Arc` and reads the totals back when
/// it snapshots. So *any* transport (moq-lite, IETF, SRT, WebRTC, ...) records
/// usage the same way just by driving the model types. An unattached model type
/// gets a fresh default `Arc<Usage>` nobody reads, so bumps stay a few cheap
/// atomics rather than a branch.
///
/// `bytes` is counted at **frame granularity**: a frame's full declared size is
/// added once at the frame boundary (when it's produced, or pulled by a
/// consumer), not as the bytes physically transfer. So a stream dropping
/// mid-frame still counts that whole frame: a bounded over-count of at most one
/// frame per interrupted stream, traded for not instrumenting every read/write.
///
/// Which side is counted is set by *where* the `Arc<Usage>` is attached:
/// - **Ingress** rides a [`crate::BroadcastProducer`] into each track/group it
///   creates. A frame is produced once, so it counts once.
/// - **Egress** rides a [`crate::BroadcastConsumer`] into each
///   [`crate::TrackSubscriber`] it hands out. Each subscriber carries its own
///   clone of the *same* `Arc<Usage>`, so N subscribers reading the same cached
///   frame each bump it (matching per-viewer egress).
#[derive(Debug, Default)]
pub struct Usage {
	groups: AtomicU64,
	frames: AtomicU64,
	bytes: AtomicU64,
}

impl Usage {
	pub(crate) fn add_group(&self) {
		self.groups.fetch_add(1, Relaxed);
	}
	pub(crate) fn add_frame(&self) {
		self.frames.fetch_add(1, Relaxed);
	}
	pub(crate) fn add_bytes(&self, n: u64) {
		self.bytes.fetch_add(n, Relaxed);
	}

	/// Total groups counted so far.
	pub fn groups(&self) -> u64 {
		self.groups.load(Relaxed)
	}
	/// Total frames counted so far.
	pub fn frames(&self) -> u64 {
		self.frames.load(Relaxed)
	}
	/// Total payload bytes counted so far.
	pub fn bytes(&self) -> u64 {
		self.bytes.load(Relaxed)
	}
}
