//! Per-broadcast usage counters carried by [`crate::BroadcastInfo`].
//!
//! [`Usage`] is a set of atomics, so the model bumps it through a shared
//! `&Arc<Usage>` with no mutation: there is no setter and no `Arc::make_mut`.
//! Each broadcast carries a [`BroadcastStats`] pair (one [`Usage`] per
//! direction) in its immutable [`crate::BroadcastInfo`], which flows down to
//! every track, group, and frame. A producer-side handle bumps the ingress
//! ([`BroadcastStats::producer`]) counter; a consumer-side handle bumps the
//! egress ([`BroadcastStats::consumer`]) one.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Cumulative usage counters for one direction of a broadcast.
///
/// `groups` / `frames` / `bytes` are payload counters bumped as media flows;
/// `opened` / `closed` are lifecycle counters bumped as handles open and close,
/// so `opened - closed` is the number of live handles. Every counter is strictly
/// monotonic (only `fetch_add`). A reader pairs an `Acquire` load of `closed`
/// with the matching `Release` bump so a snapshot never observes `closed > opened`.
#[derive(Debug, Default)]
pub struct Usage {
	groups: AtomicU64,
	frames: AtomicU64,
	bytes: AtomicU64,
	opened: AtomicU64,
	closed: AtomicU64,
}

impl Usage {
	/// Record one group.
	pub(crate) fn add_group(&self) {
		self.groups.fetch_add(1, Ordering::Relaxed);
	}

	/// Record one frame of `bytes` bytes.
	pub(crate) fn add_frame(&self, bytes: u64) {
		self.frames.fetch_add(1, Ordering::Relaxed);
		self.bytes.fetch_add(bytes, Ordering::Relaxed);
	}

	/// Record a handle opening (the live count goes up by one).
	// Wired by the model's live viewer/publisher counting (a later phase).
	#[allow(dead_code)]
	pub(crate) fn open(&self) {
		self.opened.fetch_add(1, Ordering::Relaxed);
	}

	/// Record a handle closing (the live count goes down by one).
	///
	/// `Release` so the matching `Acquire` load of `closed` in [`Self::closed`]
	/// transitively publishes the earlier `open` bump to the reader.
	#[allow(dead_code)]
	pub(crate) fn close(&self) {
		self.closed.fetch_add(1, Ordering::Release);
	}

	/// Cumulative number of groups recorded.
	pub fn groups(&self) -> u64 {
		self.groups.load(Ordering::Relaxed)
	}

	/// Cumulative number of frames recorded.
	pub fn frames(&self) -> u64 {
		self.frames.load(Ordering::Relaxed)
	}

	/// Cumulative number of payload bytes recorded.
	pub fn bytes(&self) -> u64 {
		self.bytes.load(Ordering::Relaxed)
	}

	/// Cumulative number of handles opened.
	///
	/// Load `closed` (with [`Self::closed`]) before `opened` so the readout
	/// always satisfies `opened >= closed`.
	pub fn opened(&self) -> u64 {
		self.opened.load(Ordering::Relaxed)
	}

	/// Cumulative number of handles closed. Loaded with `Acquire`; see [`Self::close`].
	pub fn closed(&self) -> u64 {
		self.closed.load(Ordering::Acquire)
	}
}

/// Usage sinks for a broadcast, one [`Usage`] per direction.
///
/// Lives in [`crate::BroadcastInfo`] so the immutable broadcast handle carries
/// the counters down to every track, group, and frame. Cloning shares the same
/// atomics (it is an `Arc` pair), so the model can bump through any clone. The
/// default is a pair of fresh, unreferenced sinks: bumps are recorded but
/// nothing reads them, so a standalone broadcast is effectively unmetered.
#[derive(Clone, Debug, Default)]
pub struct BroadcastStats {
	/// Ingress sink, bumped by producer-side handles as media is published.
	pub producer: Arc<Usage>,
	/// Egress sink, bumped by consumer-side handles as media is delivered.
	pub consumer: Arc<Usage>,
}
