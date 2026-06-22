//! HTTP server: serves HLS / LL-HLS for MoQ broadcasts.
//!
//! Routes are path-based, so one server can expose many broadcasts. The
//! broadcast name may contain slashes (e.g. `cam/lobby.hang`); the recognized
//! trailing shape decides what's being requested:
//!
//! ```text
//! GET /{broadcast}/index.m3u8                (alias: master.m3u8)
//! GET /{broadcast}/{rendition}/media.m3u8    (LL-HLS blocking reload via ?_HLS_msn=&_HLS_part=)
//! GET /{broadcast}/{rendition}/init.mp4
//! GET /{broadcast}/{rendition}/seg/{seq}.m4s
//! GET /{broadcast}/{rendition}/part/{seq}/{idx}.m4s
//! ```
//!
//! By default every broadcast is public. An embedder (e.g. a CDN edge) can pass
//! an [`Authorizer`] to [`Server::with_authorizer`] to gate each request by a
//! `?jwt=` subscribe token; the standalone binary leaves it unset.

mod routes;

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;

use crate::export::{Broadcaster, Config};

/// How long to wait for a requested broadcast to be announced by the relay.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);

/// Why an HLS request was refused, mapped to an HTTP status by the routes.
#[derive(Debug)]
pub enum AuthRejected {
	/// Missing or invalid credentials for a protected broadcast (-> 401).
	Unauthorized(String),
	/// Valid credentials, but not permitted to subscribe (-> 403).
	Forbidden(String),
}

/// The boxed future an [`Authorizer`] returns. Boxed (rather than RPITIT) so the
/// trait is object-safe and the server can hold an `Arc<dyn Authorizer>`.
pub type AuthFuture<'a> = Pin<Box<dyn Future<Output = Result<(), AuthRejected>> + Send + 'a>>;

/// Per-request authorization hook. The embedder implements this to gate each
/// broadcast by its `?jwt=` subscribe token (an absent token is a public
/// request); the standalone binary leaves it unset, so every broadcast is public.
pub trait Authorizer: Send + Sync + 'static {
	/// Authorize serving `broadcast` to a request bearing `token`. `Ok(())` allows
	/// it; an `Err` is surfaced as 401/403.
	fn authorize<'a>(&'a self, broadcast: &'a str, token: Option<&'a str>) -> AuthFuture<'a>;
}

/// HLS export HTTP server. Cheap to clone (shared inner).
#[derive(Clone)]
pub struct Server {
	inner: Arc<Inner>,
}

struct Inner {
	origin: moq_net::OriginConsumer,
	config: Config,
	broadcasters: Mutex<HashMap<String, Arc<Broadcaster>>>,
	/// Optional per-request gate. `None` => every broadcast is public.
	authorizer: Option<Arc<dyn Authorizer>>,
}

impl Server {
	/// Build a server reading broadcasts from `origin`, with no auth (every
	/// broadcast is public). Use [`with_authorizer`](Self::with_authorizer) to gate.
	pub fn new(origin: moq_net::OriginConsumer, config: Config) -> Self {
		Self::build(origin, config, None)
	}

	/// Build a server that authorizes every request through `authorizer`.
	pub fn with_authorizer(origin: moq_net::OriginConsumer, config: Config, authorizer: Arc<dyn Authorizer>) -> Self {
		Self::build(origin, config, Some(authorizer))
	}

	fn build(origin: moq_net::OriginConsumer, config: Config, authorizer: Option<Arc<dyn Authorizer>>) -> Self {
		Self {
			inner: Arc::new(Inner {
				origin,
				config,
				broadcasters: Mutex::new(HashMap::new()),
				authorizer,
			}),
		}
	}

	/// The axum router for the HLS endpoints.
	pub fn router(&self) -> Router {
		routes::router(self.clone())
	}

	/// Run the configured authorizer (if any) for `broadcast` + `token`.
	pub(crate) async fn authorize(&self, broadcast: &str, token: Option<&str>) -> Result<(), AuthRejected> {
		match &self.inner.authorizer {
			Some(authorizer) => authorizer.authorize(broadcast, token).await,
			None => Ok(()),
		}
	}

	/// Get or create the [`Broadcaster`] for `name`, resolving the broadcast from
	/// the relay (waiting briefly for its announcement). Returns `None` if the
	/// broadcast never shows up.
	///
	/// A newly created broadcaster is cached and packaged until the broadcast
	/// closes, at which point a reaper drops the cache entry (so the map stays
	/// bounded and a re-announced broadcast gets a fresh packager). There is no
	/// idle-while-live teardown: a broadcast nobody is pulling HLS for is never
	/// created, but one that's still live keeps packaging.
	pub(crate) async fn broadcaster(&self, name: &str) -> Option<Arc<Broadcaster>> {
		if let Some(existing) = self.inner.broadcasters.lock().unwrap().get(name).cloned() {
			return Some(existing);
		}

		let broadcast = tokio::time::timeout(RESOLVE_TIMEOUT, self.inner.origin.announced_broadcast(name))
			.await
			.ok()
			.flatten()?;

		let mut broadcasters = self.inner.broadcasters.lock().unwrap();
		// Double-check: another request may have inserted while we awaited.
		if let Some(existing) = broadcasters.get(name).cloned() {
			return Some(existing);
		}
		let reaper_handle = broadcast.clone();
		let broadcaster = Broadcaster::new(broadcast, self.inner.config.clone());
		broadcasters.insert(name.to_string(), broadcaster.clone());
		drop(broadcasters);

		// Reaper: once the broadcast closes, drop the cache entry. The broadcaster's
		// own catalog/pump tasks end on close too, so this just releases the map slot.
		let inner = self.inner.clone();
		let name = name.to_string();
		tokio::spawn(async move {
			kio::wait(|waiter| reaper_handle.poll_closed(waiter)).await;
			inner.broadcasters.lock().unwrap().remove(&name);
		});

		Some(broadcaster)
	}
}
