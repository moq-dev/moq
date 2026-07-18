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
//! Every request is served. To gate access, wrap [`Server::router`] in your own
//! [`axum`] middleware. It runs before routing, so a rejected request never reaches
//! the origin, but it also sees the raw request URI rather than the path parameters
//! axum decodes for the handlers. The broadcast is the first segment, still
//! percent-encoded: `/li%76e/master.m3u8` serves the broadcast `live`. Decode a
//! segment before matching it against a policy, or a name can be encoded past the
//! check.
//!
//! ```no_run
//! use axum::http::StatusCode;
//! use axum::middleware::{self, Next};
//! use axum::extract::Request;
//! use axum::response::Response;
//!
//! async fn gate(req: Request, next: Next) -> Result<Response, StatusCode> {
//!     match req.headers().get("authorization") {
//!         Some(token) if token == "secret" => Ok(next.run(req).await),
//!         _ => Err(StatusCode::UNAUTHORIZED),
//!     }
//! }
//!
//! # fn build(server: moq_hls::Server) -> axum::Router {
//! server.router().layer(middleware::from_fn(gate))
//! # }
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
	origin: moq_net::origin::Consumer,
	config: Config,
	broadcasters: Mutex<HashMap<String, Arc<Broadcaster>>>,
}

impl Server {
	/// Build a server reading broadcasts from `origin`. Every request is served;
	/// gate access by layering middleware onto [`router`](Self::router).
	pub fn new(origin: moq_net::origin::Consumer, config: Config) -> Self {
		Self {
			inner: Arc::new(Inner {
				origin,
				config,
				broadcasters: Mutex::new(HashMap::new()),
			}),
		}
	}

	/// The axum router for the HLS endpoints, ready to nest or wrap in middleware.
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

	/// Mirrors the middleware example in this module's docs, which `doctest = false`
	/// keeps out of the compiler's reach. Paused time skips the allowed request's
	/// RESOLVE_TIMEOUT wait for a broadcast that never arrives.
	#[tokio::test(start_paused = true)]
	async fn router_middleware_gates_requests() {
		use axum::body::Body;
		use axum::extract::Request;
		use axum::http::{StatusCode, header};
		use axum::middleware::{self, Next};
		use axum::response::Response;
		use tower::ServiceExt;

		async fn gate(req: Request, next: Next) -> Result<Response, StatusCode> {
			match req.headers().get(header::AUTHORIZATION) {
				Some(token) if token == "secret" => Ok(next.run(req).await),
				_ => Err(StatusCode::UNAUTHORIZED),
			}
		}

		// An origin with no broadcasts: a request that reaches the handlers 404s after
		// RESOLVE_TIMEOUT, so a 401 proves the middleware rejected it first.
		let origin = moq_net::Origin::random().produce();
		let server = Server::new(origin.consume(), Config::default());
		let app = server.router().layer(middleware::from_fn(gate));

		let denied = app
			.clone()
			.oneshot(Request::builder().uri("/live/master.m3u8").body(Body::empty()).unwrap())
			.await
			.unwrap();
		assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

		let allowed = app
			.oneshot(
				Request::builder()
					.uri("/live/master.m3u8")
					.header(header::AUTHORIZATION, "secret")
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();
		assert_eq!(allowed.status(), StatusCode::NOT_FOUND);
		// Pin the 404 to our handler: an unmatched route would also 404, but without
		// the no-store that not_found() sets, so a broken URI can't fake this pass.
		assert_eq!(allowed.headers()[header::CACHE_CONTROL], "no-store");
	}

	async fn closed_broadcaster() -> Arc<Broadcaster> {
		let origin = moq_net::Origin::random().produce();
		let producer = origin.create_broadcast("gone").expect("publish allowed");
		producer.set_live(true);
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
		_producer.set_live(true);

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
		new_producer.set_live(true);
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
