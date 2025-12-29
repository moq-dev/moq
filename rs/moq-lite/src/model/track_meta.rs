use std::ops::Deref;
use std::{
	fmt,
	future::Future,
	sync::{
		atomic::{self, AtomicUsize},
		LazyLock,
	},
};

use priority_queue::PriorityQueue;
use tokio::sync::watch;

use crate::{ExpiresProducer, Produce, Time};

#[derive(Clone, Debug, PartialEq, Eq, Copy, Default)]
pub struct TrackMeta {
	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: Time,
}

impl TrackMeta {
	pub fn produce(self) -> Produce<TrackMetaProducer, TrackMetaConsumer> {
		let producer = TrackMetaProducer::new(self);
		let consumer = producer.consume();
		Produce { producer, consumer }
	}

	pub fn max(&self, other: &Self) -> Self {
		Self {
			priority: self.priority.max(other.priority),
			max_latency: self.max_latency.max(other.max_latency),
		}
	}
}

/// Keeps track of the maximum priority and max latency of all consumers.
#[derive(Default, Debug)]
struct State {
	priority: PriorityQueue<usize, u8>,
	max_latency: PriorityQueue<usize, Time>,
}

static NEXT_ID: LazyLock<AtomicUsize> = LazyLock::new(|| AtomicUsize::new(0));

#[derive(Default)]
pub struct TrackMetaProducer {
	id: usize,
	state: watch::Sender<State>,
	current: TrackMeta,
	expires: ExpiresProducer,
}

impl fmt::Debug for TrackMetaProducer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackMetaProducer")
			.field("state", &self.state.borrow().deref())
			.field("current", &self.current)
			.finish()
	}
}

impl TrackMetaProducer {
	pub fn new(meta: TrackMeta) -> Self {
		Self::new_expires(meta, Default::default())
	}

	pub(super) fn new_expires(meta: TrackMeta, expires: ExpiresProducer) -> Self {
		let mut this = Self {
			id: NEXT_ID.fetch_add(1, atomic::Ordering::Relaxed),
			state: Default::default(),
			current: meta,
			expires,
		};
		this.set(meta);
		this
	}

	pub fn get(&self) -> TrackMeta {
		let state = self.state.borrow();

		TrackMeta {
			priority: *state.priority.peek().unwrap().1,
			max_latency: *state.max_latency.peek().unwrap().1,
		}
	}

	pub fn set(&mut self, meta: TrackMeta) {
		self.current = meta;

		self.state.send_if_modified(|state| {
			let max_max_latency = state.max_latency.peek().map(|max| max.1).copied().unwrap_or_default();
			let max_priority = state
				.priority
				.peek()
				.map(|priority| priority.1)
				.copied()
				.unwrap_or_default();

			state.priority.push(self.id, meta.priority);
			state.max_latency.push(self.id, meta.max_latency);

			if meta.max_latency > max_max_latency {
				self.expires.set_max_latency(meta.max_latency);
				true
			} else if meta.priority > max_priority {
				true
			} else {
				// Only wake up consumers if we're the new maximum for either.
				false
			}
		});
	}

	pub fn consume(&self) -> TrackMetaConsumer {
		TrackMetaConsumer {
			state: self.state.subscribe(),
		}
	}

	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let state = self.state.clone();
		async move {
			state.closed().await;
		}
	}
}

impl Clone for TrackMetaProducer {
	fn clone(&self) -> Self {
		let mut this = Self {
			id: NEXT_ID.fetch_add(1, atomic::Ordering::Relaxed),
			state: self.state.clone(),
			current: self.current,
			expires: self.expires.clone(),
		};
		this.set(self.current);
		this
	}
}

impl Drop for TrackMetaProducer {
	fn drop(&mut self) {
		self.state.send_if_modified(|state| {
			state.priority.remove(&self.id).expect("id not found");
			state.max_latency.remove(&self.id).expect("id not found");

			if let (Some((_, max_max_latency)), Some((_, max_priority))) =
				(state.max_latency.peek(), state.priority.peek())
			{
				return self.current.max_latency > *max_max_latency || self.current.priority > *max_priority;
			}

			// Always send if there are no more consumers.
			true
		});
	}
}

#[derive(Clone)]
pub struct TrackMetaConsumer {
	state: watch::Receiver<State>,
}

impl fmt::Debug for TrackMetaConsumer {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackMetaConsumer")
			.field("state", &self.state.borrow().deref())
			.finish()
	}
}

impl TrackMetaConsumer {
	pub fn get(&self) -> Option<TrackMeta> {
		let state = self.state.borrow();
		Some(TrackMeta {
			priority: *state.priority.peek()?.1,
			max_latency: *state.max_latency.peek()?.1,
		})
	}

	pub async fn changed(&mut self) -> Option<TrackMeta> {
		self.state.changed().await.ok()?;
		self.get()
	}
}
