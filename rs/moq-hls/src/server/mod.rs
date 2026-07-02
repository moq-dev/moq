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

mod routes;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;

use crate::export::{Broadcaster, Config};

/// How long to wait for a requested broadcast to be announced by the relay.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// HLS export HTTP server. Cheap to clone (shared inner).
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	origin: moq_net::OriginConsumer,
	config: Config,
	broadcasters: Mutex<HashMap<String, Arc<Broadcaster>>>,
}

impl Server {
	/// Build a server reading broadcasts from `origin`.
	pub fn new(origin: moq_net::OriginConsumer, config: Config) -> Self {
		Self {
			inner: Arc::new(Inner {
				origin,
				config,
				broadcasters: Mutex::new(HashMap::new()),
			}),
		}
	}

	/// The axum router for the HLS endpoints.
	pub fn router(&self) -> Router {
		routes::router(self.clone())
	}

	/// Get or create the [`Broadcaster`] for `name`, resolving the broadcast from
	/// the relay (waiting briefly for its announcement). Returns `None` if the
	/// broadcast never shows up.
	pub(crate) async fn broadcaster(&self, name: &str) -> Option<Arc<Broadcaster>> {
		{
			let mut broadcasters = self.inner.broadcasters.lock().unwrap();
			if let Some(existing) = broadcasters.get(name) {
				if !existing.is_closed() {
					return Some(existing.clone());
				}
				broadcasters.remove(name);
			}
		}

		let broadcast = tokio::time::timeout(RESOLVE_TIMEOUT, self.inner.origin.announced_broadcast(name))
			.await
			.ok()
			.flatten()?;

		let mut broadcasters = self.inner.broadcasters.lock().unwrap();
		if let Some(existing) = broadcasters.get(name) {
			if !existing.is_closed() {
				return Some(existing.clone());
			}
			broadcasters.remove(name);
		}

		let name = name.to_string();
		let broadcaster = Broadcaster::new(broadcast, self.inner.config.clone());
		broadcasters.insert(name.clone(), broadcaster.clone());
		tokio::spawn(evict_closed(self.inner.clone(), name, broadcaster.clone()));
		Some(broadcaster)
	}
}

async fn evict_closed(inner: Arc<Inner>, name: String, broadcaster: Arc<Broadcaster>) {
	broadcaster.closed().await;

	let mut broadcasters = inner.broadcasters.lock().unwrap();
	if broadcasters
		.get(&name)
		.is_some_and(|current| Arc::ptr_eq(current, &broadcaster))
	{
		broadcasters.remove(&name);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn closed_broadcaster() -> Arc<Broadcaster> {
		let producer = moq_net::Broadcast::new().produce();
		let broadcaster = Broadcaster::new(producer.consume(), Config::default());
		drop(producer);
		broadcaster
	}

	#[tokio::test]
	async fn broadcaster_replaces_finished_cached_instance() {
		let origin = moq_net::Origin::random().produce();
		let server = Server::new(origin.consume(), Config::default());
		let stale = closed_broadcaster();

		server
			.inner
			.broadcasters
			.lock()
			.unwrap()
			.insert("live".to_string(), stale.clone());
		let _producer = origin.create_broadcast("live").expect("publish allowed");

		let fresh = server.broadcaster("live").await.expect("broadcast announced");

		assert!(!Arc::ptr_eq(&stale, &fresh));
		assert!(server.inner.broadcasters.lock().unwrap().contains_key("live"));
	}

	#[tokio::test]
	async fn eviction_keeps_newer_cached_instance() {
		let origin = moq_net::Origin::random().produce();
		let server = Server::new(origin.consume(), Config::default());
		let old = closed_broadcaster();
		let new_producer = moq_net::Broadcast::new().produce();
		let new = Broadcaster::new(new_producer.consume(), Config::default());

		server
			.inner
			.broadcasters
			.lock()
			.unwrap()
			.insert("live".to_string(), new.clone());

		evict_closed(server.inner.clone(), "live".to_string(), old).await;

		let cached = server.inner.broadcasters.lock().unwrap().get("live").cloned();
		assert!(cached.is_some_and(|cached| Arc::ptr_eq(&cached, &new)));
		drop(new_producer);
	}
}
