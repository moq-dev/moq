use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

/// Shared payload counters for one track slot: just three atomics the model
/// bumps as media flows.
///
/// The stats layer owns one per `(broadcast, tier, role)` inside an `Arc`, hands
/// a clone to the model that's producing/consuming the track, and reads the
/// totals back when it snapshots. There's no trait or dispatch: a bump is a
/// direct `fetch_add` on the atomic, so *any* transport (moq-lite, IETF, SRT,
/// WebRTC, ...) records usage the same way just by driving the model types.
///
/// `bytes` is counted at **frame granularity**: a frame's full declared size is
/// added once at the frame boundary (when it's produced, or pulled by a
/// consumer), not as the bytes physically transfer. So a stream dropping
/// mid-frame still counts that whole frame: a bounded over-count of at most one
/// frame per interrupted stream, traded for not instrumenting every read/write.
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

/// A cheap, cloneable handle to an optional shared [`Usage`] that the model
/// carries on its producer/consumer types and bumps as data flows.
///
/// The default is a no-op (no counters attached), so a model type that isn't
/// wired to stats pays only an `Option` check per bump. Attach one with
/// [`Meter::from_arc`]; bumps then `fetch_add` on the shared atomics.
///
/// Which side is counted is set by *where* the handle is attached:
/// - **Ingress** lives on a [`crate::TrackProducer`] and propagates into each
///   [`crate::GroupProducer`] it creates. A frame is produced once, so it counts
///   once.
/// - **Egress** lives on a [`crate::TrackSubscriber`] and propagates into each
///   [`crate::GroupConsumer`] it hands out. Each subscriber carries its own
///   handle to the *same* shared `Usage`, so N subscribers reading the same
///   cached frame each bump it (matching per-viewer egress).
#[derive(Clone, Default)]
pub struct Meter(Option<Arc<Usage>>);

impl Meter {
	/// Wrap a shared [`Usage`] so the model bumps it. The stats layer keeps its
	/// own clone of the same `Arc` to read the totals.
	pub fn from_arc(usage: Arc<Usage>) -> Self {
		Self(Some(usage))
	}

	/// True when no counters are attached (every bump is a no-op).
	pub fn is_noop(&self) -> bool {
		self.0.is_none()
	}

	pub(crate) fn group(&self) {
		if let Some(usage) = &self.0 {
			usage.add_group();
		}
	}

	pub(crate) fn frame(&self) {
		if let Some(usage) = &self.0 {
			usage.add_frame();
		}
	}

	pub(crate) fn bytes(&self, n: u64) {
		if let Some(usage) = &self.0 {
			usage.add_bytes(n);
		}
	}
}

impl std::fmt::Debug for Meter {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Meter").field("attached", &self.0.is_some()).finish()
	}
}
