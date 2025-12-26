use std::future::Future;

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

#[derive(Default, Clone)]
pub struct TrackMetaProducer {
	state: watch::Sender<TrackMeta>,
}

impl TrackMetaProducer {
	pub fn new(meta: TrackMeta) -> Self {
		Self {
			state: watch::Sender::new(meta),
		}
	}

	pub fn get(&self) -> TrackMeta {
		self.state.borrow().clone()
	}

	pub fn set(&mut self, meta: TrackMeta) {
		self.state.send_if_modified(|state| {
			if *state == meta {
				return false;
			}

			*state = meta;
			true
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

#[derive(Clone)]
pub struct TrackMetaConsumer {
	state: watch::Receiver<TrackMeta>,
}

impl TrackMetaConsumer {
	pub fn get(&self) -> TrackMeta {
		self.state.borrow().clone()
	}

	pub async fn next(&mut self) -> Option<TrackMeta> {
		self.state.changed().await.ok()?;
		Some(self.state.borrow_and_update().clone())
	}
}
