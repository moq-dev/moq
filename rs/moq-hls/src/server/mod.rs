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
		if let Some(existing) = self.inner.broadcasters.lock().unwrap().get(name).cloned() {
			return Some(existing);
		}

		let broadcast = tokio::time::timeout(RESOLVE_TIMEOUT, self.inner.origin.announced_broadcast(name))
			.await
			.ok()
			.flatten()?;

		let mut broadcasters = self.inner.broadcasters.lock().unwrap();
		let broadcaster = broadcasters
			.entry(name.to_string())
			.or_insert_with(|| Broadcaster::new(broadcast, self.inner.config.clone()));
		Some(broadcaster.clone())
	}
}
