//! SRT listener configuration and the unauthenticated `run` convenience.
//!
//! SRT is a thin reliability/encryption layer over a UDP datagram stream whose
//! payload, by overwhelming convention, is MPEG-TS. [`run`] drives a [`Server`]:
//! it accepts every connection and routes each by its stream-id `m=` mode, in one
//! of two directions:
//!
//! - `m=publish` (the default): ingest. Pump the caller's TS payload into the
//!   origin as a broadcast.
//! - `m=request`: egress. Re-mux the requested broadcast back to MPEG-TS and
//!   stream it to the caller, so VLC / ffmpeg can play
//!   `srt://host:port?streamid=#!::r=<broadcast>,m=request`.
//!
//! Routing: SRT's recommended stream-id form is `#!::r=<resource>,m=<mode>`. The
//! `r=` resource (or the raw stream id, for OBS-style clients) names the
//! broadcast, and the optional [`prefix`](Config::prefix) is prepended so a
//! single listener can namespace all of its streams (e.g. prefix `live/` + stream
//! id `cam0` -> broadcast `live/cam0`).
//!
//! Auth: [`run`] is unauthenticated. Anyone who can reach the UDP port can
//! publish or request any broadcast, so gate it with the host firewall / a
//! private network. To gate access (e.g. verify the stream id as a JWT) or to
//! scope the origin per client, drive [`Server`] directly: loop on
//! [`Server::accept`], match the [`Request`], and call accept/reject after making
//! your own decision.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_net::OriginProducer;

use crate::Result;
use crate::server::{Request, Server};

/// SRT gateway configuration.
///
/// Construct via [`Config::default`] and set the fields you need, so new
/// options stay additive. The listener is disabled (and [`run`] stays pending)
/// unless [`listen`](Config::listen) is set, letting an embedding relay run
/// without SRT until it's configured.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Address to listen on for SRT (e.g. `0.0.0.0:9000`). When `None`, the SRT
	/// gateway is disabled.
	pub listen: Option<SocketAddr>,

	/// Prefix prepended to every broadcast path, for both publish and request.
	/// Lets one listener namespace all of its streams (e.g. `live/`).
	pub prefix: String,

	/// SRT receive latency: the negotiated buffer that trades delay for loss
	/// recovery.
	pub latency: Duration,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			listen: None,
			prefix: String::new(),
			latency: Duration::from_millis(200),
		}
	}
}

/// Run the SRT gateway until it fails, publishing `m=publish` connections into
/// `origin` and serving `m=request` connections out of it.
///
/// This is the unauthenticated convenience entry point: it accepts every
/// publisher and subscriber and routes by [`prefix`](Config::prefix) + resource
/// name. Subscribe requests are served from `origin.consume()`, so anything in
/// the origin (SRT ingests and otherwise) can be pulled back out over SRT. To
/// gate access (e.g. verify the stream id as a JWT) or to scope the origin per
/// client, drive [`Server`] directly.
///
/// Stays pending forever (rather than resolving) when SRT is disabled, so it
/// composes cleanly inside a `tokio::select!` alongside a relay's other
/// long-running tasks.
pub async fn run(origin: OriginProducer, config: Config) -> Result<()> {
	let Some(listen) = config.listen else {
		tracing::info!("SRT gateway disabled (no listen address)");
		std::future::pending::<()>().await;
		unreachable!("pending future never resolves");
	};

	let mut server = Server::bind(listen, config.latency).await?;
	tracing::info!(%listen, prefix = %config.prefix, "SRT listening");

	// Read side of the origin, used to serve `m=request` callers their broadcast.
	let consumer = origin.consume();

	// Tracks which broadcast paths are currently being ingested so a second
	// publisher on the same stream id is rejected (first-publisher-wins, like an
	// RTMP stream key) instead of being silently parked as a backup that could
	// take over the path when the first publisher drops.
	let active = ActivePaths::default();
	let prefix = Arc::new(config.prefix);

	while let Some(request) = server.accept().await {
		let prefix = prefix.clone();
		match request {
			Request::Publish(publish) => {
				let origin = origin.clone();
				let active = active.clone();
				// Each connection runs on its own task: `accept` pumps media for the
				// whole connection lifetime, so handling it inline would serialize
				// publishers.
				tokio::spawn(async move {
					let peer = publish.peer();
					let path = format!("{prefix}{}", publish.resource());
					// Claim the path before accepting; the guard releases it when the
					// connection task ends (success, error, or panic).
					let Some(_guard) = active.claim(&path) else {
						tracing::warn!(%peer, %path, "rejecting SRT publish: path already being ingested");
						let _ = publish.reject().await;
						return;
					};
					if let Err(err) = publish.accept(&origin, &path).await {
						tracing::warn!(%peer, %path, %err, "SRT ingest ended with error");
					} else {
						tracing::info!(%peer, %path, "SRT ingest ended");
					}
				});
			}
			Request::Subscribe(subscribe) => {
				let consumer = consumer.clone();
				// Many viewers can request the same path concurrently, so subscribes
				// don't claim an `ActivePaths` slot.
				tokio::spawn(async move {
					let peer = subscribe.peer();
					let path = format!("{prefix}{}", subscribe.resource());
					if let Err(err) = subscribe.accept(&consumer, &path).await {
						tracing::warn!(%peer, %path, %err, "SRT request ended with error");
					} else {
						tracing::info!(%peer, %path, "SRT request ended");
					}
				});
			}
		}
	}

	Err(crate::Error::from(anyhow::anyhow!(
		"SRT listener stopped accepting connections"
	)))
}

/// The set of broadcast paths with a live ingest, used to reject duplicate
/// stream ids. Cheap to clone (shared `Arc`).
#[derive(Clone, Default)]
struct ActivePaths(Arc<Mutex<HashSet<String>>>);

impl ActivePaths {
	/// Claim `path`, returning a guard that releases it on drop, or `None` if it
	/// is already claimed.
	fn claim(&self, path: &str) -> Option<PathGuard> {
		let mut set = self.0.lock().expect("active paths mutex poisoned");
		set.insert(path.to_string()).then(|| PathGuard {
			paths: self.0.clone(),
			path: path.to_string(),
		})
	}
}

/// Releases a claimed [`ActivePaths`] entry when dropped.
struct PathGuard {
	paths: Arc<Mutex<HashSet<String>>>,
	path: String,
}

impl Drop for PathGuard {
	fn drop(&mut self) {
		self.paths
			.lock()
			.expect("active paths mutex poisoned")
			.remove(&self.path);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn active_paths_rejects_duplicates_and_releases_on_drop() {
		let active = ActivePaths::default();

		let guard = active.claim("live/cam0").expect("first claim succeeds");
		// A second claim of the same path is rejected while the first is held.
		assert!(active.claim("live/cam0").is_none());
		// A different path is unaffected.
		let other = active.claim("live/cam1").expect("distinct path claims");

		// Dropping the guard releases the path so it can be reclaimed.
		drop(guard);
		assert!(active.claim("live/cam0").is_some());

		drop(other);
	}
}
