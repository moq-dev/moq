use std::sync::{
	Arc,
	atomic::{AtomicBool, Ordering},
};

/// A waker that queues its key, once per burst of wakes, for the owner's next pass.
pub(crate) struct QueueWake<K> {
	key: K,
	/// Set while queued, so a burst of wakes queues the key once.
	queued: AtomicBool,
	queue: kio::Queue<K>,
}

impl<K> QueueWake<K> {
	pub fn new(key: K, queue: kio::Queue<K>) -> Arc<Self> {
		Arc::new(Self {
			key,
			queued: AtomicBool::new(false),
			queue,
		})
	}

	/// Unmark the key once popped, before polling, so a wake landing mid-pass queues it again.
	pub fn popped(&self) {
		self.queued.store(false, Ordering::Release);
	}
}

impl<K: Clone + Send + Sync + 'static> std::task::Wake for QueueWake<K> {
	fn wake(self: Arc<Self>) {
		self.wake_by_ref();
	}

	fn wake_by_ref(self: &Arc<Self>) {
		if !self.queued.swap(true, Ordering::AcqRel) {
			let _ = self.queue.try_push(self.key.clone());
		}
	}
}
