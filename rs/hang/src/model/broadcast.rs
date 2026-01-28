use std::{
	ops::{Deref, DerefMut},
	sync::{Arc, atomic},
};

use crate::{
	TrackConsumer,
	catalog::{Catalog, CatalogConsumer, CatalogProducer},
};

#[derive(Clone)]
pub struct BroadcastProducer {
	pub inner: moq_lite::BroadcastProducer,
	pub catalog: CatalogProducer,

	track_id: Arc<atomic::AtomicUsize>,
}

impl BroadcastProducer {
	pub fn new(mut inner: moq_lite::BroadcastProducer) -> Self {
		let catalog = Catalog::default().produce();
		inner.insert_track(catalog.track.clone());

		Self {
			inner,
			catalog,
			track_id: Default::default(),
		}
	}

	// A helper to generate a unique track name.
	pub fn track_name(&self, prefix: &str) -> String {
		let track_id = self.track_id.fetch_add(1, atomic::Ordering::Relaxed);
		format!("{}{}", prefix, track_id)
	}
}

impl Default for BroadcastProducer {
	fn default() -> Self {
		Self::new(moq_lite::BroadcastProducer::default())
	}
}

impl Deref for BroadcastProducer {
	type Target = moq_lite::BroadcastProducer;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl DerefMut for BroadcastProducer {
	fn deref_mut(&mut self) -> &mut Self::Target {
		&mut self.inner
	}
}

impl From<moq_lite::BroadcastProducer> for BroadcastProducer {
	fn from(inner: moq_lite::BroadcastProducer) -> Self {
		Self::new(inner)
	}
}

impl From<BroadcastProducer> for moq_lite::BroadcastProducer {
	fn from(producer: BroadcastProducer) -> Self {
		producer.inner
	}
}

#[derive(Clone)]
pub struct BroadcastConsumer {
	pub inner: moq_lite::BroadcastConsumer,
	pub catalog: CatalogConsumer,
}

impl BroadcastConsumer {
	pub fn new(inner: moq_lite::BroadcastConsumer) -> Self {
		let catalog = inner.subscribe_track(&Catalog::default_track()).into();
		Self { inner, catalog }
	}

	pub fn subscribe(&self, track: &moq_lite::Track, latency: std::time::Duration) -> TrackConsumer {
		TrackConsumer::new(self.inner.subscribe_track(track), latency)
	}
}

impl Deref for BroadcastConsumer {
	type Target = moq_lite::BroadcastConsumer;

	fn deref(&self) -> &Self::Target {
		&self.inner
	}
}

impl From<moq_lite::BroadcastConsumer> for BroadcastConsumer {
	fn from(inner: moq_lite::BroadcastConsumer) -> Self {
		Self::new(inner)
	}
}
