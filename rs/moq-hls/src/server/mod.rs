//! HTTP server: serves HLS / LL-HLS for MoQ broadcasts.
//!
//! Routes are path-based, so one server can expose many broadcasts:
//!
//! ```text
//! GET /{broadcast}/master.m3u8
//! GET /{broadcast}/{rendition}/media.m3u8   (LL-HLS blocking reload via ?_HLS_msn=&_HLS_part=)
//! GET /{broadcast}/{rendition}/init.mp4
//! GET /{broadcast}/{rendition}/seg/{seq}.m4s
//! GET /{broadcast}/{rendition}/part/{seq}/{idx}.m4s
//! ```
//!
//! `{broadcast}` may span several path components, since MoQ broadcast names are
//! hierarchical (`room/user`); the endpoint is matched from the end of the path.

mod routes;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;

use crate::export::{Broadcaster, Config, Handle};

/// How long to wait for a requested broadcast to be announced by the relay.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Aborts the driver task when its cache entry is dropped, tearing the whole
/// export (catalog consumer + every exporter) down and releasing its source
/// subscriptions -- so evicting/replacing an entry stops recording immediately.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
	fn drop(&mut self) {
		self.0.abort();
	}
}

/// One cached broadcast: the read handle served to HTTP requests, plus the driver
/// task that owns the `Broadcaster` (aborted when this entry is dropped). The `id`
/// distinguishes generations so a finishing driver only evicts its OWN entry, not a
/// replacement.
struct Entry {
	id: u64,
	handle: Handle,
	_driver: AbortOnDrop,
}

/// HLS export HTTP server. Cheap to clone (shared inner).
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	origin: moq_net::OriginConsumer,
	config: Config,
	broadcasters: Mutex<HashMap<String, Entry>>,
	next_id: AtomicU64,
}

impl Server {
	/// Build a server reading broadcasts from `origin`.
	pub fn new(origin: moq_net::OriginConsumer, config: Config) -> Self {
		Self {
			inner: Arc::new(Inner {
				origin,
				config,
				broadcasters: Mutex::new(HashMap::new()),
				next_id: AtomicU64::new(0),
			}),
		}
	}

	/// The axum router for the HLS endpoints.
	pub fn router(&self) -> Router {
		routes::router(self.clone())
	}

	/// Get or create the read [`Handle`] for `name`, resolving the broadcast from the
	/// relay (waiting briefly for its announcement) and spawning a driver to fill it.
	/// Returns `None` if the broadcast never shows up (or its catalog can't be
	/// subscribed).
	pub(crate) async fn handle(&self, name: &str) -> Option<Handle> {
		if let Some(handle) = self.cached(name) {
			return Some(handle);
		}

		let broadcast = tokio::time::timeout(RESOLVE_TIMEOUT, self.inner.origin.announced_broadcast(name))
			.await
			.ok()
			.flatten()?;

		let mut broadcasters = self.inner.broadcasters.lock().unwrap();
		// Someone may have resolved the same broadcast while we awaited.
		if let Some(entry) = broadcasters.get(name) {
			return Some(entry.handle.clone());
		}

		let mut broadcaster = match Broadcaster::new(broadcast, self.inner.config.clone()) {
			Ok(broadcaster) => broadcaster,
			Err(err) => {
				tracing::warn!(%name, %err, "failed to start HLS export");
				return None;
			}
		};
		let handle = broadcaster.handle();
		let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
		let inner = self.inner.clone();
		let name = name.to_string();
		let driver = tokio::spawn({
			let name = name.clone();
			async move {
				broadcaster.run().await;
				// Self-evict once the broadcast has finished, unless this entry has
				// already been replaced by a newer generation.
				evict(&inner, &name, id);
			}
		});
		broadcasters.insert(
			name,
			Entry {
				id,
				handle: handle.clone(),
				_driver: AbortOnDrop(driver.abort_handle()),
			},
		);
		Some(handle)
	}

	/// Return the cached handle for `name`, if one is live.
	fn cached(&self, name: &str) -> Option<Handle> {
		let broadcasters = self.inner.broadcasters.lock().unwrap();
		broadcasters.get(name).map(|entry| entry.handle.clone())
	}
}

/// Remove `name`'s cache entry iff it is still generation `id` (a driver that just
/// finished doesn't evict a replacement that took its place).
fn evict(inner: &Inner, name: &str, id: u64) {
	let mut broadcasters = inner.broadcasters.lock().unwrap();
	if broadcasters.get(name).is_some_and(|entry| entry.id == id) {
		broadcasters.remove(name);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A broadcast producer carrying a `catalog.json` track, so a `Broadcaster` can
	/// subscribe its catalog (fail-loud `new` rejects a broadcast without one). The
	/// producer + track are returned so the caller keeps the broadcast alive.
	fn live_broadcast() -> (moq_net::BroadcastProducer, moq_net::TrackProducer) {
		let mut producer = moq_net::Broadcast::new().produce();
		let catalog = producer
			.create_track(moq_net::Track {
				name: "catalog.json".to_string(),
				priority: 0,
			})
			.unwrap();
		(producer, catalog)
	}

	/// Seed an entry of generation `id` over a live broadcast; returns the guards that
	/// keep it alive (producer + catalog track).
	fn seed_entry(server: &Server, id: u64) -> (moq_net::BroadcastProducer, moq_net::TrackProducer) {
		let (producer, catalog) = live_broadcast();
		let mut broadcaster = Broadcaster::new(producer.consume(), Config::default()).unwrap();
		let handle = broadcaster.handle();
		let driver = tokio::spawn(async move { broadcaster.run().await });
		server.inner.broadcasters.lock().unwrap().insert(
			"live".to_string(),
			Entry {
				id,
				handle,
				_driver: AbortOnDrop(driver.abort_handle()),
			},
		);
		(producer, catalog)
	}

	#[tokio::test]
	async fn resolves_and_caches_handle() {
		let origin = moq_net::Origin::random().produce();
		let server = Server::new(origin.consume(), Config::default());
		let mut producer = origin.create_broadcast("live").expect("publish allowed");
		let _catalog = producer
			.create_track(moq_net::Track {
				name: "catalog.json".to_string(),
				priority: 0,
			})
			.unwrap();

		let first = server.handle("live").await.expect("broadcast announced");
		// A second request reuses the same cached entry rather than spawning a new driver.
		assert!(server.inner.broadcasters.lock().unwrap().contains_key("live"));
		let _second = server.handle("live").await.expect("cached");
		assert_eq!(server.inner.broadcasters.lock().unwrap().len(), 1);
		drop(first);
	}

	#[tokio::test]
	async fn finished_driver_evicts_its_own_entry() {
		let origin = moq_net::Origin::random().produce();
		let server = Server::new(origin.consume(), Config::default());
		let _guards = seed_entry(&server, 7);

		evict(&server.inner, "live", 7);
		assert!(!server.inner.broadcasters.lock().unwrap().contains_key("live"));
	}

	#[tokio::test]
	async fn stale_eviction_keeps_newer_entry() {
		let origin = moq_net::Origin::random().produce();
		let server = Server::new(origin.consume(), Config::default());
		// The current entry is generation 9.
		let _guards = seed_entry(&server, 9);

		// An older generation (id 8) finishing must NOT evict the newer entry.
		evict(&server.inner, "live", 8);
		assert!(server.inner.broadcasters.lock().unwrap().contains_key("live"));
	}
}
