//! FFI wrapper over [`moq_net::Cache`]: a shared RAM LRU group cache.
//!
//! Construct a [`MoqCache`] from a [`MoqCacheConfig`] (byte budget + wall-clock age) and attach
//! it when creating a broadcast (or an origin, which cascades to the broadcasts it creates). Many
//! broadcasts/tracks can share one [`MoqCache`] handle and draw from a single budget. Without an
//! explicit cache, an FFI broadcast gets a default one (see [`MoqCacheConfig::default`]) so the
//! common publish -> consume path isn't silently lossy under load.

use std::sync::Arc;
use std::time::Duration;

/// Configuration for a [`MoqCache`]: the shared byte budget and the wall-clock age bound.
///
/// Mirrors [`moq_net::cache::Config`]. The age bound is expressed in milliseconds at the FFI
/// boundary. Defaults to a 64 MiB budget and a 5s window, the same retention the publish path
/// uses when no cache is attached.
#[derive(Clone, Copy, uniffi::Record)]
pub struct MoqCacheConfig {
	/// Maximum total bytes retained across every track sharing this cache. The
	/// least-recently-accessed groups are evicted once the total would exceed this.
	#[uniffi(default = 67108864)]
	pub max_bytes: u64,
	/// Maximum wall-clock age, in milliseconds, since a group was last accessed before it is
	/// evicted (least-recently-accessed first).
	#[uniffi(default = 5000)]
	pub max_age_ms: u64,
}

impl Default for MoqCacheConfig {
	fn default() -> Self {
		Self {
			max_bytes: DEFAULT_MAX_BYTES,
			max_age_ms: DEFAULT_MAX_AGE.as_millis() as u64,
		}
	}
}

/// Default shared-cache byte budget for an FFI broadcast: 64 MiB.
///
/// The age bound is what actually governs retention for normal media; `max_bytes` is a RAM
/// ceiling so a misconfigured huge window can't grow unbounded. 64 MiB comfortably holds a few
/// seconds of audio/video.
pub(crate) const DEFAULT_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Default shared-cache age window for an FFI broadcast, matching [`moq_net::DEFAULT_CACHE`].
pub(crate) const DEFAULT_MAX_AGE: Duration = moq_net::DEFAULT_CACHE;

/// A shared, cheaply cloneable handle to a RAM LRU group cache.
///
/// Attach it when creating a broadcast or origin; clone the handle (via [`Self::clone_handle`])
/// to share one budget across many of them. See [`moq_net::Cache`] for the eviction policy.
#[derive(uniffi::Object)]
pub struct MoqCache {
	inner: moq_net::Cache,
}

impl MoqCache {
	/// The wrapped moq-net cache handle (a cheap `Arc` clone).
	pub(crate) fn inner(&self) -> moq_net::Cache {
		self.inner.clone()
	}
}

#[uniffi::export]
impl MoqCache {
	/// Create a cache with the given [`MoqCacheConfig`].
	#[uniffi::constructor]
	pub fn new(config: MoqCacheConfig) -> Arc<Self> {
		let inner = moq_net::Cache::new(
			moq_net::cache::Config::default()
				.with_max_bytes(config.max_bytes)
				.with_max_age(Duration::from_millis(config.max_age_ms)),
		);
		Arc::new(Self { inner })
	}

	/// A handle sharing this cache's budget. Attach the clone elsewhere to pool retention.
	pub fn clone_handle(&self) -> Arc<Self> {
		Arc::new(Self {
			inner: self.inner.clone(),
		})
	}

	/// Whether two handles share the same underlying budget (one is a clone of the other).
	pub fn is_clone(&self, other: &MoqCache) -> bool {
		self.inner.is_clone(&other.inner)
	}
}

/// The default cache attached to an FFI broadcast when the caller provides none.
pub(crate) fn default_cache() -> moq_net::Cache {
	moq_net::Cache::new(
		moq_net::cache::Config::default()
			.with_max_bytes(DEFAULT_MAX_BYTES)
			.with_max_age(DEFAULT_MAX_AGE),
	)
}
