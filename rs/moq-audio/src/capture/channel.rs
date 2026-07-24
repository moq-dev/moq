//! A bounded, non-blocking channel for audio capture callbacks.

use std::sync::{
	Arc,
	atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, mpsc};

/// Roughly 320 ms at the common 10 ms callback cadence.
const DEPTH: usize = 32;
const DROP_REPORT_INTERVAL: Duration = Duration::from_secs(1);

pub(super) fn bounded<T>() -> (Sender<T>, Receiver<T>) {
	let (tx, rx) = mpsc::channel(DEPTH);
	let shared = Arc::new(Shared {
		closed: AtomicBool::new(false),
		dropped: AtomicU64::new(0),
		closed_notify: Notify::new(),
	});

	(
		Sender {
			tx,
			shared: shared.clone(),
		},
		Receiver {
			rx,
			shared,
			last_drop_report: None,
		},
	)
}

pub(super) struct Sender<T> {
	tx: mpsc::Sender<T>,
	shared: Arc<Shared>,
}

impl<T> Sender<T> {
	/// Enqueue without waiting, dropping the newest buffer when the queue is full.
	pub(super) fn push(&self, item: T) {
		if self.shared.closed.load(Ordering::Acquire) {
			return;
		}

		match self.tx.try_send(item) {
			Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
			Err(mpsc::error::TrySendError::Full(_)) => {
				self.shared.dropped.fetch_add(1, Ordering::Relaxed);
			}
		}
	}

	/// Stop accepting buffers and wake the reader once queued audio is drained.
	pub(super) fn close(&self) {
		self.shared.closed.store(true, Ordering::Release);
		self.shared.closed_notify.notify_waiters();
	}
}

pub(super) struct Receiver<T> {
	rx: mpsc::Receiver<T>,
	shared: Arc<Shared>,
	last_drop_report: Option<Instant>,
}

impl<T> Receiver<T> {
	pub(super) async fn recv(&mut self) -> Option<T> {
		let item = loop {
			// Register before checking the flag so a concurrent close cannot be
			// missed between the check and the await.
			let closed = self.shared.closed_notify.notified();
			match self.rx.try_recv() {
				Ok(item) => break Some(item),
				Err(mpsc::error::TryRecvError::Disconnected) => break None,
				Err(mpsc::error::TryRecvError::Empty) => {}
			}
			if self.shared.closed.load(Ordering::Acquire) {
				break None;
			}

			tokio::select! {
				item = self.rx.recv() => break item,
				_ = closed => {}
			}
		};

		self.report_drops();
		item
	}

	fn report_drops(&mut self) {
		if self.shared.dropped.load(Ordering::Relaxed) == 0 {
			return;
		}

		let now = Instant::now();
		if self
			.last_drop_report
			.is_some_and(|last| now.duration_since(last) < DROP_REPORT_INTERVAL)
		{
			return;
		}

		let dropped = self.shared.dropped.swap(0, Ordering::Relaxed);
		self.last_drop_report = Some(now);
		tracing::warn!(dropped, capacity = DEPTH, "dropped audio capture buffers");
	}
}

struct Shared {
	closed: AtomicBool,
	dropped: AtomicU64,
	closed_notify: Notify,
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
		tx.close();

		let mut received = Vec::new();
		while let Some(id) = rx.recv().await {
			received.push(id);
		}

		assert_eq!(received, (0..DEPTH).collect::<Vec<_>>());
	}

	#[tokio::test]
	async fn close_returns_none_after_draining() {
		let (tx, mut rx) = bounded();
		tx.push(1);
		tx.close();

		assert_eq!(rx.recv().await, Some(1));
		assert_eq!(rx.recv().await, None);
	}
}
