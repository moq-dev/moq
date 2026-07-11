//! HTTP server: serves HLS for MoQ broadcasts, fetching media on demand.
//!
//! Routes are path-based, so one server can expose many broadcasts:
//!
//! ```text
//! GET /{broadcast}/master.m3u8
//! GET /{broadcast}/{kind}/{rendition}/media.m3u8
//! GET /{broadcast}/{kind}/{rendition}/init.mp4
//! GET /{broadcast}/{kind}/{rendition}/seg/{group}.m4s
//! ```
//!
//! `{kind}` is `video` or `audio`, so a video and an audio rendition that share a
//! name address distinct resources.
//!
//! By default every request is served. Pass an [`Authorizer`] via
//! [`Server::with_authorizer`] to gate access per request (token, cookie, or
//! signed URL); a denied request is rejected before the origin is touched.

mod routes;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, StatusCode};

use crate::export::{Broadcaster, Config};

/// How long to wait for a requested broadcast to be announced by the relay.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-request authorization for the HLS endpoints. Return `Err(status)` to deny
/// a request; the server never touches the origin for a denied request. A closure
/// `Fn(&str, &HeaderMap, Option<&str>) -> Result<(), StatusCode>` implements this.
pub trait Authorizer: Send + Sync + 'static {
	/// `broadcast` is the requested broadcast path, `headers` the request headers
	/// (Authorization / Cookie), `query` the raw query string (token / signed-URL schemes).
	fn authorize(&self, broadcast: &str, headers: &HeaderMap, query: Option<&str>) -> Result<(), StatusCode>;
}

impl<F> Authorizer for F
where
	F: Fn(&str, &HeaderMap, Option<&str>) -> Result<(), StatusCode> + Send + Sync + 'static,
{
	fn authorize(&self, broadcast: &str, headers: &HeaderMap, query: Option<&str>) -> Result<(), StatusCode> {
		self(broadcast, headers, query)
	}
}

/// HLS export HTTP server. Cheap to clone (shared inner).
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	origin: moq_net::origin::Consumer,
	config: Config,
	broadcasters: Mutex<HashMap<String, Arc<Broadcaster>>>,
	/// Optional per-request authorizer, set at most once via [`Server::with_authorizer`];
	/// unset allows every request. On the shared inner so a clone or an
	/// already-handed-out router sees it too (no auth-bypass from call ordering).
	auth: OnceLock<Arc<dyn Authorizer>>,
}

impl Server {
	/// Build a server reading broadcasts from `origin`. Every request is allowed;
	/// call [`with_authorizer`](Self::with_authorizer) to gate access.
	pub fn new(origin: moq_net::origin::Consumer, config: Config) -> Self {
		Self {
			inner: Arc::new(Inner {
				origin,
				config,
				broadcasters: Mutex::new(HashMap::new()),
				auth: OnceLock::new(),
			}),
		}
	}

	/// Gate every request through `auth`, rejecting a denied request before the server
	/// touches the origin. The authorizer lives on the shared inner, so every clone of
	/// this server and its router enforces it regardless of call order. Set at most
	/// once; a second call is ignored.
	pub fn with_authorizer(self, auth: impl Authorizer) -> Self {
		let _ = self.inner.auth.set(Arc::new(auth));
		self
	}

	/// Authorize a request, allowing it when no authorizer is configured.
	pub(crate) fn authorize(
		&self,
		broadcast: &str,
		headers: &HeaderMap,
		query: Option<&str>,
	) -> Result<(), StatusCode> {
		match self.inner.auth.get() {
			Some(auth) => auth.authorize(broadcast, headers, query),
			None => Ok(()),
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

		// Confirm the broadcast is announced (and in scope) before building a broadcaster;
		// `Broadcaster::new` re-resolves it through the origin, which also lets a rendition's
		// catalog `broadcast` field reference a sibling broadcast.
		tokio::time::timeout(RESOLVE_TIMEOUT, self.inner.origin.announced_broadcast(name))
			.await
			.ok()
			.flatten()?;

		let source = moq_mux::Source::new(self.inner.origin.consume(), name);
		let broadcaster = Broadcaster::new(source, self.inner.config.clone())
			.await
			.map_err(|err| tracing::warn!(%err, %name, "failed to resolve broadcast catalog"))
			.ok()?;

		let mut broadcasters = self.inner.broadcasters.lock().unwrap();
		if let Some(existing) = broadcasters.get(name) {
			if !existing.is_closed() {
				return Some(existing.clone());
			}
			broadcasters.remove(name);
		}

		let name = name.to_string();
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

	async fn closed_broadcaster() -> Arc<Broadcaster> {
		let origin = moq_net::Origin::random().produce();
		let producer = origin.create_broadcast("gone").expect("publish allowed");
		let source = moq_mux::Source::new(origin.consume(), "gone");
		let broadcaster = Broadcaster::new(source, Config::default())
			.await
			.expect("catalog broadcast resolves while announced");
		// Drop the publisher so the resolved broadcast (and the broadcaster) reports closed.
		drop(producer);
		broadcaster
	}

	#[tokio::test]
	async fn broadcaster_replaces_finished_cached_instance() {
		let origin = moq_net::Origin::random().produce();
		let server = Server::new(origin.consume(), Config::default());
		let stale = closed_broadcaster().await;

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
		let old = closed_broadcaster().await;
		let new_producer = origin.create_broadcast("live").expect("publish allowed");
		let new = Broadcaster::new(moq_mux::Source::new(origin.consume(), "live"), Config::default())
			.await
			.expect("catalog broadcast resolves while announced");

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
