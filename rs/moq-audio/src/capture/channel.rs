//! A bounded handoff from a realtime capture callback to the async capture loop.
//!
//! [`Sender::push`] never blocks: when the reader falls behind, the newest
//! buffer is dropped and counted. The reader observes that as a
//! [`gap`](Receiver::gap) and re-anchors its timeline, since the encoder's PTS
//! advances by sample count and would otherwise compress the missing audio out
//! and drift behind wall clock forever.

use std::sync::{
	Arc,
	atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

/// Roughly 80 ms at the common 10 ms callback cadence. The camera path bounds
/// its queue at 4 frames for the same reason: a deeper queue is latency the
/// reader can never claw back, and audio only needs enough slack to ride out a
/// scheduling hiccup.
const DEPTH: usize = 8;

/// Drops are logged at most this often, so a sustained stall doesn't spam.
const REPORT_INTERVAL: Duration = Duration::from_secs(1);

/// Create the queue. Dropping the [`Sender`] closes it, so a parked
/// [`recv`](Receiver::recv) drains and then returns `None`.
pub(super) fn bounded<T>() -> (Sender<T>, Receiver<T>) {
	let (tx, rx) = mpsc::channel(DEPTH);
	let dropped = Arc::new(AtomicU64::new(0));

	let sender = Sender {
		tx,
		dropped: dropped.clone(),
	};
	let receiver = Receiver {
		rx,
		dropped,
		gap: false,
		unreported: 0,
		last_report: None,
	};

	(sender, receiver)
}

/// The realtime end of the queue.
pub(super) struct Sender<T> {
	tx: mpsc::Sender<T>,
	dropped: Arc<AtomicU64>,
}

impl<T> Sender<T> {
	/// Enqueue without ever blocking, dropping the newest item when the queue is
	/// full. Drop-newest keeps the buffers already queued for the in-order
	/// encoder; either choice leaves the same gap.
	pub(super) fn push(&self, item: T) {
		match self.tx.try_send(item) {
			// A closed queue means the reader is gone, i.e. we're shutting down.
			Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
			Err(mpsc::error::TrySendError::Full(_)) => {
				self.dropped.fetch_add(1, Ordering::Relaxed);
			}
		}
	}
}

/// The async end of the queue.
pub(super) struct Receiver<T> {
	rx: mpsc::Receiver<T>,
	dropped: Arc<AtomicU64>,
	gap: bool,
	unreported: u64,
	last_report: Option<Instant>,
}

impl<T> Receiver<T> {
	/// Await the next item, or `None` once the [`Sender`] is gone and the queue is
	/// drained. Cancel-safe: drop the future to stop reading.
	pub(super) async fn recv(&mut self) -> Option<T> {
		let item = self.rx.recv().await;
		self.observe();
		item
	}

	/// Whether anything was dropped since the last call, i.e. the item just
	/// returned is not contiguous with the previous one. Clears the flag.
	pub(super) fn gap(&mut self) -> bool {
		std::mem::take(&mut self.gap)
	}

	/// Fold any drops into the gap flag and the throttled log.
	fn observe(&mut self) {
		let dropped = self.dropped.swap(0, Ordering::Relaxed);
		if dropped > 0 {
			self.gap = true;
			self.unreported += dropped;
		}

		if self.unreported == 0 {
			return;
		}

		let now = Instant::now();
		if self
			.last_report
			.is_some_and(|last| now.duration_since(last) < REPORT_INTERVAL)
		{
			return;
		}

		tracing::warn!(
			dropped = self.unreported,
			capacity = DEPTH,
			"dropped audio capture buffers"
		);
		self.last_report = Some(now);
		self.unreported = 0;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn drops_newest_when_full() {
		let (tx, mut rx) = bounded();
		for id in 0..DEPTH + 2 {
			tx.push(id);
		}
		drop(tx);

		let mut received = Vec::new();
		while let Some(id) = rx.recv().await {
			received.push(id);
		}

		// The queue keeps what was already lined up for the encoder.
		assert_eq!(received, (0..DEPTH).collect::<Vec<_>>());
	}

	#[tokio::test]
	async fn drop_marks_a_gap() {
		let (tx, mut rx) = bounded();
		for id in 0..DEPTH + 1 {
			tx.push(id);
		}

		// The drop happened while these were queued, so it surfaces on the first
		// read that observes it and only once.
		assert_eq!(rx.recv().await, Some(0));
		assert!(rx.gap());
		assert_eq!(rx.recv().await, Some(1));
		assert!(!rx.gap());
	}

	#[tokio::test]
	async fn no_gap_without_drops() {
		let (tx, mut rx) = bounded();
		tx.push(1);

		assert_eq!(rx.recv().await, Some(1));
		assert!(!rx.gap());
	}

	#[tokio::test]
	async fn close_returns_none_after_draining() {
		let (tx, mut rx) = bounded();
		tx.push(1);
		drop(tx);

		// Buffered items drain first, then `None` signals the source ended.
		assert_eq!(rx.recv().await, Some(1));
		assert_eq!(rx.recv().await, None);
	}
}
