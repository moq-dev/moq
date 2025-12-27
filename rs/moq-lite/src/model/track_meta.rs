use std::{
	future::Future,
	sync::{
		atomic::{self, AtomicUsize},
		LazyLock,
	},
};

use priority_queue::PriorityQueue;
use tokio::sync::watch;

use crate::Produce;

#[derive(Clone, Debug, PartialEq, Eq, Copy, Default)]
pub struct TrackMeta {
	/// Higher priority tracks will be served first during congestion.
	pub priority: u8,

	/// Groups will be dropped if they are this much older than the latest group.
	pub max_latency: std::time::Duration,
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
#[derive(Default)]
struct State {
	priority: PriorityQueue<usize, u8>,
	max_latency: PriorityQueue<usize, std::time::Duration>,
}

static NEXT_ID: LazyLock<AtomicUsize> = LazyLock::new(|| AtomicUsize::new(0));

#[derive(Default)]
pub struct TrackMetaProducer {
	id: usize,
	state: watch::Sender<State>,
	current: TrackMeta,
}

impl TrackMetaProducer {
	pub fn new(meta: TrackMeta) -> Self {
		let mut this = Self {
			id: NEXT_ID.fetch_add(1, atomic::Ordering::Relaxed),
			state: Default::default(),
			current: meta,
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

			// Only wake up consumers if we're the new maximum for either.
			meta.priority > max_priority || meta.max_latency > max_max_latency
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
