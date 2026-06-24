use std::time::Duration;

use crate::{Error, Id, NonZeroSlab};

/// Default shared-cache byte budget for a broadcast: 64 MiB.
///
/// The age window is what governs retention for normal media; `max_bytes` is a RAM ceiling so a
/// misconfigured huge window can't grow unbounded. 64 MiB comfortably holds a few seconds of
/// audio/video.
pub(crate) const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Default shared-cache age window, matching [`moq_net::DEFAULT_CACHE`].
pub(crate) const DEFAULT_MAX_AGE: Duration = moq_net::DEFAULT_CACHE;

/// Build a [`moq_net::Cache`] from a byte budget and an age window in milliseconds.
pub(crate) fn build(max_bytes: u64, max_age_ms: u64) -> moq_net::Cache {
	moq_net::Cache::new(
		moq_net::cache::Config::default()
			.with_max_bytes(max_bytes)
			.with_max_age(Duration::from_millis(max_age_ms)),
	)
}

/// The cache attached to a broadcast/origin when the caller passes no explicit handle.
pub(crate) fn default_cache() -> moq_net::Cache {
	moq_net::Cache::new(
		moq_net::cache::Config::default()
			.with_max_bytes(DEFAULT_MAX_BYTES)
			.with_max_age(DEFAULT_MAX_AGE),
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
