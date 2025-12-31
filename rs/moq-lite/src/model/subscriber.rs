use std::cmp::Reverse;
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

use crate::{Delivery, Time};

/// Keeps track of the maximum priority and max latency of all consumers.
#[derive(Default, Debug)]
struct Max {
	priority: PriorityQueue<usize, u8>,
	max_latency: PriorityQueue<usize, Time>,

	// We prefered `ordered: false`, because those subscribers are more latency sensitive.
	ordered: PriorityQueue<usize, Reverse<bool>>,
}

static NEXT_ID: LazyLock<AtomicUsize> = LazyLock::new(|| AtomicUsize::new(0));

pub struct Subscriber {
	id: usize,
	max: watch::Sender<Max>,
	current: Delivery,
}

impl fmt::Debug for Subscriber {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackMetaProducer")
			.field("state", &self.max.borrow().deref())
			.field("current", &self.current)
			.finish()
	}
}

impl Subscriber {
	pub fn new(delivery: Delivery) -> Self {
		let id = NEXT_ID.fetch_add(1, atomic::Ordering::Relaxed);
		let max = watch::Sender::new(Max::default());

		let mut this = Self {
			id,
			max: max.clone(),
			current: Default::default(),
		};
		this.update(delivery);
		this
	}

	pub fn current(&self) -> &Delivery {
		&self.current
	}

	pub fn update(&mut self, delivery: Delivery) {
		self.current = delivery;

		self.max.send_if_modified(|state| {
			let old_max_latency = state.max_latency.peek().map(|max| max.1).copied().unwrap_or_default();
			let old_priority = state
				.priority
				.peek()
				.map(|priority| priority.1)
				.copied()
				.unwrap_or_default();
			let old_ordered = state
				.ordered
				.peek()
				.map(|ordered| ordered.1)
				.copied()
				.unwrap_or_default();

			state.priority.push(self.id, delivery.priority);
			state.max_latency.push(self.id, delivery.max_latency);
			state.ordered.push(self.id, Reverse(delivery.ordered));

			delivery.max_latency > old_max_latency
				|| delivery.priority > old_priority
				|| delivery.ordered != old_ordered.0
		});
	}

	// We don't use the `async` keyword so we don't borrow &self across the await.
	pub fn unused(&self) -> impl Future<Output = ()> {
		let state = self.max.clone();
		async move {
			state.closed().await;
		}
	}
}

impl Drop for Subscriber {
	fn drop(&mut self) {
		self.max.send_if_modified(|state| {
			let old_max_latency = state.max_latency.peek().map(|max| max.1).copied().unwrap_or_default();
			let old_priority = state
				.priority
				.peek()
				.map(|priority| priority.1)
				.copied()
				.unwrap_or_default();
			let old_ordered = state
				.ordered
				.peek()
				.map(|ordered| ordered.1)
				.copied()
				.unwrap_or_default();

			state.priority.remove(&self.id).expect("id not found");
			state.max_latency.remove(&self.id).expect("id not found");
			state.ordered.remove(&self.id).expect("id not found");

			if let (Some((_, new_max_latency)), Some((_, new_priority)), Some((_, new_ordered))) =
				(state.max_latency.peek(), state.priority.peek(), state.ordered.peek())
			{
				return old_max_latency > *new_max_latency
					|| old_priority > *new_priority
					|| old_ordered != *new_ordered;
			}

			// Always send if there are no more consumers.
			true
		});
	}
}

#[derive(Clone)]
pub struct Subscribers {
	max: (watch::Sender<Max>, watch::Receiver<Max>),
}

impl fmt::Debug for Subscribers {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_struct("TrackMetaConsumer")
			.field("max", &self.max.1.borrow().deref())
			.finish()
	}
}

impl Subscribers {
	pub fn new() -> Self {
		Self {
			max: watch::channel(Max::default()),
		}
	}

	pub fn max(&self) -> Option<Delivery> {
		let state = self.max.1.borrow();
		Some(Delivery {
			priority: *state.priority.peek()?.1,
			max_latency: *state.max_latency.peek()?.1,
			ordered: state.ordered.peek()?.1 .0,
		})
	}

	pub async fn changed(&mut self) -> Option<Delivery> {
		self.max.1.changed().await.ok()?;
		self.max()
	}

	pub fn subscribe(&self, delivery: Delivery) -> Subscriber {
		let id = NEXT_ID.fetch_add(1, atomic::Ordering::Relaxed);
		let max = self.max.0.clone();

		let mut this = Subscriber {
			id,
			max,
			current: Default::default(),
		};
		this.update(delivery);
		this
	}
}

impl Default for Subscribers {
	fn default() -> Self {
		Self::new()
	}
}
