//! HTTP server: serves HLS and DASH for MoQ broadcasts, fetching media on demand.
//!
//! Routes are path-based, so one server can expose many broadcasts:
//!
//! ```text
//! GET /{broadcast}/master.m3u8
//! GET /{broadcast}/manifest.mpd
//! GET /{broadcast}/{kind}/{rendition}/media.m3u8
//! GET /{broadcast}/{kind}/{rendition}/init.{hash}.mp4
//! GET /{broadcast}/{kind}/{rendition}/seg/{reference}.{segment}.m4s
//! GET /{broadcast}/{kind}/{rendition}/seg/{reference}.t{pts}.m4s
//! ```
//!
//! `{kind}` is `video` or `audio`, so a video and an audio rendition that share a
//! name address distinct resources. The two `seg/` forms name the same bytes: HLS
//! playlists address a segment by its aligned number, the DASH manifest by its
//! timeline `pts` (`$Time$`). `{hash}` is a hash of the init segment's bytes, so a
//! reconfigured rendition gets a new init URL. A broadcaster given a generation
//! ([`Broadcaster::set_generation`](crate::export::Broadcaster::set_generation))
//! prefixes every segment file with it (`seg/{generation}.{reference}.{segment}.m4s`), and
//! refuses segment URLs carrying any other.
//!
//! Every request is served. To gate access, wrap [`Server::router`] in your own
//! [`axum`] middleware. It runs before routing, so a rejected request never reaches
//! the origin, but it also sees the raw request URI rather than the path segments
//! the server decodes. A broadcast occupies every segment before the HLS resource
//! suffix, still percent-encoded: `/project/li%76e/master.m3u8` serves the broadcast
//! `project/live`. Decode each segment before matching it against a policy, or a
//! name can be encoded past the check.
//!
//! An embedder that authorizes each request itself (a token scoping the broadcast, a
//! stats-metered origin per tenant) skips the router instead: parse the path with
//! [`Route::parse`](crate::export::Route::parse), check
//! [`Route::broadcast`](crate::export::Route::broadcast), rewrite it relative to the scope, and
//! answer with [`Server::respond`] on a `Server` built from that scope's origin. The
//! decoded broadcast is what a policy should check.
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
use std::sync::{Arc, Mutex, Weak};
use std::task::Poll;
use std::time::Duration;

use axum::Router;

use crate::export::{Broadcaster, Config};

/// How long to wait for the relay to say whether a requested broadcast is announced.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// HLS export HTTP server. Cheap to clone (shared inner).
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	origin: moq_net::origin::Consumer,
	/// Watches the whole scope while the server lives, so a lazily discovering client
	/// session asks its peer once rather than once per requested path, which an HTTP
	/// client could otherwise name without bound.
	_watch: moq_net::announce::Consumer,
	config: Config,
	broadcasters: Mutex<HashMap<String, Arc<Broadcaster>>>,
	/// Closes with the last server clone, ending every eviction task, which would otherwise keep
	/// its broadcaster for as long as the publisher stays live.
	alive: kio::Producer<()>,
}

impl Server {
	/// Build a server reading broadcasts from `origin`, which it watches whole while it
	/// lives. Every request is served; gate access by layering middleware onto
	/// [`router`](Self::router).
	pub fn new(origin: moq_net::origin::Consumer, config: Config) -> Self {
		Self {
			inner: Arc::new(Inner {
				_watch: origin.clone().with_hidden(true).announced(),
				origin,
				config,
				broadcasters: Mutex::new(HashMap::new()),
				alive: kio::Producer::new(()),
			}),
		}
	}

	/// The axum router for the HLS endpoints, ready to nest or wrap in middleware.
	pub fn router(&self) -> Router {
		routes::router(self.clone())
	}

	/// Get or create the [`Broadcaster`] for `name`, resolving the broadcast from
	/// the relay. Returns `None` if the broadcast is not announced once the origin has
	/// synced with the relay.
	pub async fn broadcaster(&self, name: &str) -> Option<Arc<Broadcaster>> {
		{
			let mut broadcasters = self.inner.broadcasters.lock().unwrap();
			if let Some(existing) = broadcasters.get(name) {
				if !existing.is_closed() {
					return Some(existing.clone());
				}
				broadcasters.remove(name);
			}
		}

		// Confirm a route covers the broadcast (and it is in scope) before resolving it. The
		// source resolves a rendition's sibling `broadcast` reference through the same origin.
		// Once the origin has synced, a path with no route is not announced, so answer now
		// rather than wait out a publisher that has gone.
		let mut follow = self.inner.origin.follow(name).ok()?;
		let routed = kio::wait(|waiter| match follow.poll_next(waiter) {
			Poll::Ready(Some(moq_net::announce::Event::End(_)) | None) => Poll::Ready(false),
			Poll::Ready(Some(_)) => Poll::Ready(true),
			Poll::Pending => follow.poll_synced(waiter).map(|()| false),
		});
		if !moq_clock::timeout(RESOLVE_TIMEOUT, routed).await.unwrap_or(false) {
			return None;
		}

		let origin = self.inner.origin.consume();
		// A covering dynamic route may never answer, so the request is bounded too.
		let broadcast = moq_clock::timeout(RESOLVE_TIMEOUT, origin.request_broadcast(name, None))
			.await
			.map_err(|_| tracing::warn!(%name, "timed out resolving broadcast"))
			.ok()?
			.map_err(|err| tracing::warn!(%err, %name, "failed to resolve broadcast"))
			.ok()?;
		let broadcaster = Broadcaster::new(moq_mux::Source::new(origin, broadcast), self.inner.config.clone());

		let mut broadcasters = self.inner.broadcasters.lock().unwrap();
		if let Some(existing) = broadcasters.get(name) {
			if !existing.is_closed() {
				return Some(existing.clone());
			}
			broadcasters.remove(name);
		}

		let name = name.to_string();
		broadcasters.insert(name.clone(), broadcaster.clone());
		let alive = self.inner.alive.consume();
		tokio::spawn(evict_closed(
			Arc::downgrade(&self.inner),
			alive,
			name,
			broadcaster.clone(),
		));
		Some(broadcaster)
	}
}

/// Forget `broadcaster` once its source closes, unless the server is dropped first. The task
/// holds the server weakly, so it never keeps the server's state alive.
async fn evict_closed(inner: Weak<Inner>, alive: kio::Consumer<()>, name: String, broadcaster: Arc<Broadcaster>) {
	let source_closed = kio::wait(|waiter| {
		if broadcaster.poll_closed(waiter).is_ready() {
			return Poll::Ready(true);
		}
		alive.poll_closed(waiter).map(|()| false)
	})
	.await;
	let Some(inner) = inner.upgrade().filter(|_| source_closed) else {
		return;
	};

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
	/// Build an origin producer, spawning its driver on the ambient runtime.
	fn produce_origin() -> moq_net::origin::Producer {
		let (producer, driver) = moq_net::origin::Producer::new(moq_net::origin::Config::default());
		if tokio::runtime::Handle::try_current().is_ok() {
			tokio::spawn(moq_net::time::run(driver));
		} else {
			// A sync test: nothing polls the driver, and dropping it would tear
			// the origin down, so leak it and rely on the synchronous half.
			std::mem::forget(driver);
		}
		producer
	}

	use super::*;

	/// Let the origin's spawned attach/detach tasks run: a created broadcast
	/// becomes routable (and a finished one unroutable) asynchronously.
	async fn settle() {
		for _ in 0..10 {
			tokio::task::yield_now().await;
		}
	}

	/// Mirrors the middleware example in this module's docs, which `doctest = false`
	/// keeps out of the compiler's reach.
	#[tokio::test]
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

		// An origin with no broadcasts: a request that reaches the handlers 404s, so a 401
		// proves the middleware rejected it first.
		let origin = produce_origin();
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

	#[tokio::test]
	async fn router_matches_multisegment_broadcast_when_nested() {
		use axum::body::Body;
		use axum::extract::Request;
		use axum::http::{StatusCode, header};
		use tower::ServiceExt;

		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let app = Router::new().nest("/hls", server.router());

		let response = app
			.oneshot(
				Request::builder()
					.uri("/hls/project/live/master.m3u8")
					.body(Body::empty())
					.unwrap(),
			)
			.await
			.unwrap();

		assert_eq!(response.status(), StatusCode::NOT_FOUND);
		// The handler adds no-store. Axum's unmatched-route 404 does not.
		assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
	}

	/// A synced origin with no route for the path answers at once, rather than waiting out
	/// RESOLVE_TIMEOUT for a publisher that is not there. Paused time would skip that wait,
	/// so any elapsed time means it happened.
	#[tokio::test(start_paused = true)]
	async fn unannounced_broadcast_answers_without_waiting() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());

		let start = moq_clock::now();
		assert!(server.broadcaster("live").await.is_none());
		assert_eq!(start.elapsed(), Duration::ZERO);
	}

	async fn closed_broadcaster() -> Arc<Broadcaster> {
		let origin = produce_origin();
		let producer = origin.create_broadcast("gone").expect("publish allowed");
		producer.announce(Default::default()).expect("publish allowed");
		settle().await;
		let broadcaster = Broadcaster::new(crate::resolved(&origin, "gone").await, Config::default());
		// Close the publisher so the resolved broadcast (and the broadcaster) reports closed.
		producer.close();
		settle().await;
		broadcaster
	}

	#[tokio::test]
	async fn broadcaster_replaces_finished_cached_instance() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let stale = closed_broadcaster().await;

		server
			.inner
			.broadcasters
			.lock()
			.unwrap()
			.insert("live".to_string(), stale.clone());
		let _producer = origin.create_broadcast("live").expect("publish allowed");
		_producer.announce(Default::default()).expect("publish allowed");
		settle().await;

		let fresh = server.broadcaster("live").await.expect("broadcast announced");

		assert!(!Arc::ptr_eq(&stale, &fresh));
		assert!(server.inner.broadcasters.lock().unwrap().contains_key("live"));
	}

	/// Dropping the last server clone releases its broadcasters while their publisher stays
	/// live, every time a server is replaced, but a broadcaster a response still holds keeps
	/// serving.
	#[tokio::test]
	async fn dropping_the_server_releases_its_broadcasters() {
		let origin = produce_origin();
		let producer = origin.create_broadcast("live").expect("publish allowed");
		producer.announce(Default::default()).expect("publish allowed");
		settle().await;

		for _ in 0..4 {
			let server = Server::new(origin.consume(), Config::default());
			let broadcaster = server.broadcaster("live").await.expect("broadcast announced");
			let released = Arc::downgrade(&broadcaster);
			drop(broadcaster);
			drop(server);
			settle().await;
			assert_eq!(released.strong_count(), 0, "the dropped server kept its broadcaster");
		}

		let server = Server::new(origin.consume(), Config::default());
		let held = server.broadcaster("live").await.expect("broadcast announced");
		drop(server);
		settle().await;
		assert_eq!(Arc::strong_count(&held), 1, "only the response holds it now");
		assert!(!held.is_closed(), "a held broadcaster keeps serving");
		producer.close();
	}

	#[tokio::test]
	async fn eviction_keeps_newer_cached_instance() {
		let origin = produce_origin();
		let server = Server::new(origin.consume(), Config::default());
		let old = closed_broadcaster().await;
		let new_producer = origin.create_broadcast("live").expect("publish allowed");
		new_producer.announce(Default::default()).expect("publish allowed");
		settle().await;
		let new = Broadcaster::new(crate::resolved(&origin, "live").await, Config::default());

		server
			.inner
			.broadcasters
			.lock()
			.unwrap()
			.insert("live".to_string(), new.clone());

		let alive = server.inner.alive.consume();
		evict_closed(Arc::downgrade(&server.inner), alive, "live".to_string(), old).await;

		let cached = server.inner.broadcasters.lock().unwrap().get("live").cloned();
		assert!(cached.is_some_and(|cached| Arc::ptr_eq(&cached, &new)));
		new_producer.close();
	}
}
