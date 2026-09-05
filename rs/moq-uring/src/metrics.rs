//! Per-worker counters: buffer-pool health, batch effectiveness, ring traffic,
//! and scheduling.
//!
//! A worker is a thread that never yields to anything an ops surface can see,
//! so these are how its health leaves the thread. Every write is a relaxed
//! atomic add on the worker's own thread and every read is a relaxed load from
//! whichever thread scrapes, which makes a [`Snapshot`] a cheap, slightly
//! skewed reading rather than a consistent instant. Rates and ratios are what
//! these are for; two counters in one snapshot may be a few operations apart.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// One cumulative counter.
#[derive(Default)]
pub(crate) struct Counter(AtomicU64);

impl Counter {
	pub(crate) fn add(&self, count: u64) {
		// Relaxed: the writer is one thread and readers want a magnitude, not
		// an ordering against the operation being counted.
		self.0.fetch_add(count, Ordering::Relaxed);
	}

	fn get(&self) -> u64 {
		self.0.load(Ordering::Relaxed)
	}
}

/// The counters themselves, shared by the worker, its sockets, its timer heap,
/// and its park word.
#[derive(Default)]
pub(crate) struct Counters {
	pub rx_datagrams: Counter,
	pub rx_receives: Counter,
	pub rx_enobufs: Counter,
	pub rx_exhausted: Counter,
	pub tx_datagrams: Counter,
	pub tx_sends: Counter,
	pub tx_stalls: Counter,
	pub submissions: Counter,
	pub completions: Counter,
	pub enters: Counter,
	pub parks: Counter,
	pub wakes: Counter,
	pub timers_armed: Counter,
	pub timers_fired: Counter,
	pub timers_cancelled: Counter,
}

/// A worker's counters, readable from any thread.
///
/// Hand one to [`crate::Config::metrics`] to keep a copy the process can scrape
/// while the worker runs, or take the worker's own through
/// [`crate::Handle::metrics`]. Clones share one set of counters, so give each
/// worker its own.
#[derive(Clone, Default)]
pub struct Metrics(Arc<Counters>);

impl Metrics {
	pub(crate) fn counters(&self) -> &Arc<Counters> {
		&self.0
	}

	pub(crate) fn from_counters(counters: Arc<Counters>) -> Self {
		Self(counters)
	}

	/// Read every counter.
	pub fn snapshot(&self) -> Snapshot {
		Snapshot {
			rx_datagrams: self.0.rx_datagrams.get(),
			rx_receives: self.0.rx_receives.get(),
			rx_enobufs: self.0.rx_enobufs.get(),
			rx_exhausted: self.0.rx_exhausted.get(),
			tx_datagrams: self.0.tx_datagrams.get(),
			tx_sends: self.0.tx_sends.get(),
			tx_stalls: self.0.tx_stalls.get(),
			submissions: self.0.submissions.get(),
			completions: self.0.completions.get(),
			enters: self.0.enters.get(),
			parks: self.0.parks.get(),
			wakes: self.0.wakes.get(),
			timers_armed: self.0.timers_armed.get(),
			timers_fired: self.0.timers_fired.get(),
			timers_cancelled: self.0.timers_cancelled.get(),
		}
	}
}

impl std::fmt::Debug for Metrics {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		self.snapshot().fmt(f)
	}
}

/// A reading of one worker's counters, all cumulative since it started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Snapshot {
	/// UDP datagrams received, counting each `UDP_GRO` coalesced segment.
	pub rx_datagrams: u64,
	/// Receive completions that delivered datagrams. `rx_datagrams` over this
	/// is the GRO coalescing actually achieved.
	pub rx_receives: u64,
	/// Receives the kernel ended with `ENOBUFS`: the provided-buffer ring was
	/// empty, so no buffer could be selected and the receive was never
	/// performed. The datagram stays in the socket queue, so this is
	/// receive-side backpressure rather than a confirmed loss; sustained, it
	/// becomes one, once the socket buffer fills. The first thing to look at
	/// when throughput sags.
	pub rx_enobufs: u64,
	/// Re-arms that found no free receive buffer at all, so the socket was left
	/// unarmed until a packet released one. The pool is at its ceiling and
	/// every buffer is held by an unread packet.
	pub rx_exhausted: u64,
	/// UDP datagrams sent, counting each `UDP_SEGMENT` segment of a GSO train.
	pub tx_datagrams: u64,
	/// `sendmsg` operations staged. `tx_datagrams` over this is the GSO
	/// batching actually achieved.
	pub tx_sends: u64,
	/// Send-buffer acquisitions that found the pool drained at its ceiling and
	/// had to wait. Send-side backpressure.
	pub tx_stalls: u64,
	/// Submission queue entries the kernel accepted.
	pub submissions: u64,
	/// Completion queue entries dispatched.
	pub completions: u64,
	/// `io_uring_enter` calls. Datagrams over this is the syscall amortization
	/// the runtime exists for.
	pub enters: u64,
	/// Times the worker parked in `io_uring_enter` with nothing left to poll.
	pub parks: u64,
	/// `futex` wakes another thread had to issue because the worker was parked.
	pub wakes: u64,
	/// Timers armed, re-arms included (a re-arm is a cancel plus an arm).
	pub timers_armed: u64,
	/// Timers that reached their deadline.
	pub timers_fired: u64,
	/// Timers dropped or re-armed before their deadline.
	pub timers_cancelled: u64,
}

impl Snapshot {
	/// Timers currently in the heap: armed, less those fired and cancelled.
	pub fn timers_active(&self) -> u64 {
		self.timers_armed
			.saturating_sub(self.timers_fired)
			.saturating_sub(self.timers_cancelled)
	}
}
