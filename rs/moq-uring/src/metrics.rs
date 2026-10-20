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

/// One fixed distribution, updating one bucket per observation.
#[derive(Default)]
struct Distribution {
	buckets: [Counter; 11],
	sum: Counter,
}

impl Distribution {
	fn record(&self, value: u64) {
		let bucket = Histogram::BOUNDS
			.iter()
			.position(|bound| bound.is_none_or(|bound| value <= bound))
			.unwrap();
		self.buckets[bucket].add(1);
		self.sum.add(value);
	}

	fn snapshot(&self) -> Histogram {
		let mut count = 0;
		Histogram {
			buckets: std::array::from_fn(|index| {
				count += self.buckets[index].get();
				count
			}),
			sum: self.sum.get(),
		}
	}
}

/// Cumulative observations of queue entries per ring enter.
///
/// Like the counters, bucket and sum reads may be a few operations apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Histogram {
	/// Cumulative counts at each boundary in [`Self::BOUNDS`].
	pub buckets: [u64; 11],
	/// Queue entries across all observations.
	pub sum: u64,
}

impl Histogram {
	/// Inclusive bucket bounds, with `None` representing infinity.
	pub const BOUNDS: [Option<u64>; 11] = [
		Some(0),
		Some(1),
		Some(2),
		Some(4),
		Some(8),
		Some(16),
		Some(32),
		Some(64),
		Some(128),
		Some(256),
		None,
	];

	/// The number of enters observed, including those with no entries.
	pub fn count(&self) -> u64 {
		self.buckets[10]
	}
}

/// The counters themselves, shared by the worker, its sockets, its timers,
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
	pub turns: Counter,
	pub sq_full_enters: Counter,
	sqes: Distribution,
	cqes: Distribution,
	pub parks: Counter,
	pub wakes: Counter,
	pub timers_armed: Counter,
	pub timers_fired: Counter,
	pub timers_cancelled: Counter,
}

impl Counters {
	/// Account for one enter and the entries it accepted or made visible.
	pub(crate) fn entered(&self, submitted: usize, completed: usize) {
		self.enters.add(1);
		self.submissions.add(submitted as u64);
		self.sqes.record(submitted as u64);
		self.cqes.record(completed as u64);
	}
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
			turns: self.0.turns.get(),
			sq_full_enters: self.0.sq_full_enters.get(),
			sqes: self.0.sqes.snapshot(),
			cqes: self.0.cqes.snapshot(),
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
	/// Times the send-buffer pool became drained at its ceiling and an
	/// acquisition had to wait. Send-side backpressure.
	pub tx_stalls: u64,
	/// Submission queue entries the kernel accepted.
	pub submissions: u64,
	/// Completion queue entries dispatched.
	pub completions: u64,
	/// `io_uring_enter` calls. Datagrams over this is the syscall amortization
	/// the runtime exists for.
	pub enters: u64,
	/// Drive-loop turns, including the final poll that completes the root future.
	pub turns: u64,
	/// Enters forced by a full submission queue.
	pub sq_full_enters: u64,
	/// Submission entries accepted by each enter, including zero.
	pub sqes: Histogram,
	/// Completion entries made visible by each enter, including zero.
	pub cqes: Histogram,
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn enter_distributions_include_empty_and_overflow_buckets() {
		let metrics = Metrics::default();
		for count in [0, 1, 2, 3, 4, 255, 256, 257, 2048] {
			metrics.0.entered(count, 2048 - count);
		}
		let snapshot = metrics.snapshot();
		assert_eq!(snapshot.enters, 9);
		assert_eq!(snapshot.sqes.buckets, [1, 2, 3, 5, 5, 5, 5, 5, 5, 7, 9]);
		assert_eq!(snapshot.sqes.count(), snapshot.enters);
		assert_eq!(snapshot.cqes.count(), snapshot.enters);
		assert_eq!(snapshot.sqes.sum, snapshot.submissions);
		assert_eq!(snapshot.cqes.sum + snapshot.sqes.sum, 9 * 2048);
	}
}
