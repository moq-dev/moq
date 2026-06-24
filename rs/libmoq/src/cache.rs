use std::time::Duration;

use crate::{Error, Id, NonZeroSlab};

/// Build a [`moq_net::Cache`] from a byte budget and an age window in milliseconds. A `max_bytes`
/// of `0` means no byte cap; eviction is by age alone.
pub(crate) fn build(max_bytes: u64, max_age_ms: u64) -> moq_net::Cache {
	moq_net::Cache::new(
		moq_net::cache::Config::default()
			.with_max_bytes(max_bytes)
			.with_max_age(Duration::from_millis(max_age_ms)),
	)
}

/// Shared RAM LRU group caches handed out to C callers as opaque ids.
#[derive(Default)]
pub struct Cache {
	caches: NonZeroSlab<moq_net::Cache>,
}

impl Cache {
	/// Register a new cache with the given budget, returning its handle.
	pub fn create(&mut self, max_bytes: u64, max_age_ms: u64) -> Result<Id, Error> {
		self.caches.insert(build(max_bytes, max_age_ms))
	}

	/// Resolve a cache handle to a cheap clone of the shared `moq_net::Cache`.
	pub fn get(&self, id: Id) -> Result<moq_net::Cache, Error> {
		self.caches.get(id).cloned().ok_or(Error::NotFound)
	}

	/// Drop a cache handle. The underlying budget stays alive while any broadcast/track that was
	/// attached to it is still live (they each hold a clone).
	pub fn close(&mut self, id: Id) -> Result<(), Error> {
		self.caches.remove(id).ok_or(Error::NotFound)?;
		Ok(())
	}
}
