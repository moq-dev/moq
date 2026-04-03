use std::collections::HashMap;
use std::marker::PhantomData;
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use serde::de::DeserializeOwned;

use crate::Result;

use super::Section;

/// A catalog reader that distributes per-section change notifications.
///
/// When new JSON is fed via `update()`, each registered section is diffed
/// against its previous value. Only sections whose values actually changed
/// get their consumers notified.
#[derive(Clone)]
pub struct CatalogReader {
	inner: Arc<Mutex<CatalogReaderInner>>,
}

struct CatalogReaderInner {
	sections: HashMap<String, conducer::Producer<Option<serde_json::Value>>>,
	last: serde_json::Map<String, serde_json::Value>,
}

impl Default for CatalogReader {
	fn default() -> Self {
		Self::new()
	}
}

impl CatalogReader {
	pub fn new() -> Self {
		Self {
			inner: Arc::new(Mutex::new(CatalogReaderInner {
				sections: HashMap::new(),
				last: serde_json::Map::new(),
			})),
		}
	}

	/// Register interest in a section. Returns a `SectionConsumer<T>`.
	///
	/// If the section was already seen in a previous update, the consumer
	/// will immediately have the current value available.
	pub fn section<T: DeserializeOwned>(&self, section: &Section<T>) -> SectionConsumer<T> {
		let mut inner = self.inner.lock().unwrap();
		let name = section.name.to_string();
		let initial = inner.last.get(section.name).cloned();
		let producer = inner
			.sections
			.entry(name)
			.or_insert_with(|| conducer::Producer::new(initial));
		SectionConsumer {
			consumer: producer.consume(),
			_phantom: PhantomData,
		}
	}

	/// Feed a raw JSON map into the reader.
	///
	/// For each registered section, diffs against the previous value.
	/// Only updates (and notifies) sections whose values actually changed.
	pub fn update(&self, json: serde_json::Map<String, serde_json::Value>) {
		let mut inner = self.inner.lock().unwrap();

		for (name, producer) in &inner.sections {
			let new_value = json.get(name).cloned();
			let old_value = inner.last.get(name).cloned();

			if new_value != old_value {
				if let Ok(mut guard) = producer.write() {
					*guard = new_value;
				}
			}
		}

		inner.last = json;
	}

	/// Close all section producers, notifying consumers.
	pub fn close(&self) {
		let inner = self.inner.lock().unwrap();
		for producer in inner.sections.values() {
			let _ = producer.close();
		}
	}
}

/// A consumer for a single catalog section, backed by conducer.
///
/// Provides typed access to the section's current value and async
/// notification when the value changes. Automatically unregisters
/// when dropped (via conducer ref counting).
pub struct SectionConsumer<T> {
	consumer: conducer::Consumer<Option<serde_json::Value>>,
	_phantom: PhantomData<T>,
}

impl<T: DeserializeOwned> SectionConsumer<T> {
	/// Get the current value of this section.
	pub fn get(&self) -> Result<Option<T>> {
		let state = self.consumer.read();
		match state.deref() {
			Some(value) => Ok(Some(serde_json::from_value(value.clone())?)),
			None => Ok(None),
		}
	}

	/// Wait for the next change to this section, returning the new value.
	///
	/// Returns `Ok(None)` if the section was removed.
	/// Returns `Err` if the catalog was closed.
	pub async fn changed(&self) -> Result<Option<T>> {
		let mut first = true;
		let value: Option<serde_json::Value> = self
			.consumer
			.wait(|state| {
				if first {
					first = false;
					Poll::Pending
				} else {
					Poll::Ready(state.deref().clone())
				}
			})
			.await
			.map_err(|_| crate::Error::Closed)?;

		match value {
			Some(v) => Ok(Some(serde_json::from_value(v)?)),
			None => Ok(None),
		}
	}

	/// Wait until this section's catalog is closed.
	pub async fn closed(&self) {
		self.consumer.closed().await;
	}
}

impl<T> Clone for SectionConsumer<T> {
	fn clone(&self) -> Self {
		Self {
			consumer: self.consumer.clone(),
			_phantom: PhantomData,
		}
	}
}
