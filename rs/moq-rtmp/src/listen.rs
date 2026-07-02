//! RTMP listener, configuration, and stream-key routing.
//!
//! We run a TCP listener on RTMP's port, hand each connection to
//! [`crate::server`] for the handshake + session handling, and route each publish
//! or play to a broadcast path derived from its RTMP app and stream key.
//!
//! Routing: the usual OBS/ffmpeg/player URL is `rtmp://host[:1935]/<app>/<key>`,
//! which arrives as app `<app>` and stream key `<key>`. The path is `<app>/<key>`
//! (just `<app>` when the key is empty). The optional [`prefix`](Config::prefix)
//! is prepended so a single listener can namespace everything (e.g. prefix
//! `live/` + app `cam0` -> broadcast `live/cam0`). The same routing applies to
//! both directions: a publish ingests *into* that path, a play serves *from* it,
//! so the same URL round-trips (push to `rtmp://host/live/cam0`, pull it back from
//! the same URL).
//!
//! Auth: this listener is currently unauthenticated. Anyone who can reach the
//! TCP port can publish or play, so gate it with the host firewall / a private
//! network. Treating the stream key as a token (a moq-token JWT, as moq-edge
//! does) is the obvious next step.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use moq_net::OriginProducer;

use crate::Result;
use crate::server::{Request, Server};

/// RTMP ingest configuration.
///
/// Construct via [`Config::default`] and set the fields you need, so new
/// options stay additive. Ingest is disabled (and [`run`] stays pending) unless
/// [`listen`](Config::listen) is set, letting an embedding relay run without
/// RTMP until it's configured.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct Config {
	/// Address to listen on for RTMP ingest (e.g. `0.0.0.0:1935`). When `None`,
	/// RTMP ingest is disabled.
	pub listen: Option<SocketAddr>,

	/// Prefix prepended to every ingested broadcast path. Lets one listener
	/// namespace all of its streams (e.g. `live/`).
	pub prefix: String,

	/// How long a play's FLV muxer waits for a stalled group before skipping to a
	/// newer one (the moq-level frame-drop latency). Zero (the default) drops
	/// stale groups aggressively. Only affects egress (plays); ingest ignores it.
	pub latency: Duration,

	/// TLS configuration for RTMPS (RTMP over TLS). When set, the
	/// [`listen`](Self::listen) address speaks RTMPS instead of plaintext RTMP,
	/// so clients connect with `rtmps://`. Build it with
	/// `moq_native::tls::Server::server_config` (pass an empty ALPN list) or
	/// any [`rustls::ServerConfig`]. Leave `None` for plaintext.
	///
	/// To serve both RTMP and RTMPS, run two listeners: call [`run`] once per
	/// config (one with `tls`, one without) against a cloned origin.
	#[cfg(feature = "tls")]
	pub tls: Option<std::sync::Arc<rustls::ServerConfig>>,
}

/// Run the RTMP listener until it fails, bridging each connection to `origin`:
/// publishers ingest into it as broadcasts, players are served broadcasts from it.
///
/// This is the unauthenticated convenience entry point: it accepts every
/// publisher and player and routes by [`prefix`](Config::prefix) + app/key. Play
/// requests are served from `origin.consume()`, so anything in the origin (RTMP
/// ingests and otherwise) can be pulled back out over RTMP. To gate access (e.g.
/// verify the stream key as a JWT) or to scope the origin per client, drive
/// [`Server`] directly: loop on [`Server::accept`], match the [`Request`], and
/// call accept/reject after making your own decision.
///
/// Stays pending forever (rather than resolving) when RTMP is disabled, so it
/// composes cleanly inside a `tokio::select!` alongside a relay's other
/// long-running tasks.
pub async fn run(origin: OriginProducer, config: Config) -> Result<()> {
	let Some(listen) = config.listen else {
		tracing::info!("RTMP ingest disabled (no listen address)");
		std::future::pending::<()>().await;
		unreachable!("pending future never resolves");
	};

	#[cfg_attr(not(feature = "tls"), allow(unused_mut))]
	let mut server = Server::bind(listen).await?;

	#[cfg(feature = "tls")]
	let tls = config.tls.is_some();
	#[cfg(not(feature = "tls"))]
	let tls = false;

	#[cfg(feature = "tls")]
	if let Some(tls) = config.tls.clone() {
		server = server.with_tls(tls);
	}

	tracing::info!(%listen, prefix = %config.prefix, tls, "RTMP ingest listening");

	// Tracks which broadcast paths are currently being ingested so a second
	// publisher on the same stream key is rejected (first-publisher-wins) instead
	// of clobbering the live one.
	let active = ActivePaths::default();
	let prefix = Arc::new(config.prefix);
	let latency = config.latency;
	// Players are served out of the same origin the publishers write into.
	let consumer = origin.consume();

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
					let Some(path) = resolve_path(&prefix, publish.app(), publish.stream_key()) else {
						tracing::warn!(%peer, "rejecting RTMP publish: no usable broadcast path");
						let _ = publish.reject("missing broadcast path (RTMP app/key)").await;
						return;
					};
					// Claim the path before accepting; the guard releases it when the
					// connection task ends (success, error, or panic).
					let Some(_guard) = active.claim(&path) else {
						tracing::warn!(%peer, %path, "rejecting RTMP publish: path already being published");
						let _ = publish.reject("path already being published").await;
						return;
					};
					if let Err(err) = publish.accept(&origin, &path).await {
						tracing::warn!(%peer, %path, %err, "RTMP ingest ended with error");
					}
				});
			}
			Request::Play(play) => {
				let consumer = consumer.clone();
				// Many viewers can play the same path concurrently, so plays don't
				// claim an `ActivePaths` slot.
				tokio::spawn(async move {
					let peer = play.peer();
					let Some(path) = resolve_path(&prefix, play.app(), play.stream_key()) else {
						tracing::warn!(%peer, "rejecting RTMP play: no usable broadcast path");
						let _ = play.reject("missing broadcast path (RTMP app/key)").await;
						return;
					};
					if let Err(err) = play.with_latency(latency).accept(&consumer, &path).await {
						tracing::warn!(%peer, %path, %err, "RTMP play ended with error");
					}
				});
			}
		}
	}

	Err(anyhow::anyhow!("RTMP listener stopped accepting connections").into())
}

/// Derive a broadcast path from an RTMP app and stream key, applying `prefix`.
///
/// `rtmp://host/<app>/<key>` maps to `<prefix><app>/<key>`, falling back to just
/// the app (or just the key) when the other half is empty. Returns `None` when
/// there's nothing usable to route on.
pub(crate) fn resolve_path(prefix: &str, app: &str, key: &str) -> Option<String> {
	let app = app.trim_matches('/').trim();
	let key = key.trim_matches('/').trim();
	let name = match (app.is_empty(), key.is_empty()) {
		(true, true) => return None,
		(false, true) => app.to_string(),
		(true, false) => key.to_string(),
		(false, false) => format!("{app}/{key}"),
	};
	Some(format!("{prefix}{name}"))
}

/// The set of broadcast paths with a live ingest, used to reject duplicate
/// stream keys. Cheap to clone (shared `Arc`).
#[derive(Clone, Default)]
pub(crate) struct ActivePaths(Arc<Mutex<HashSet<String>>>);

impl ActivePaths {
	/// Claim `path`, returning a guard that releases it on drop, or `None` if it
	/// is already claimed.
	pub(crate) fn claim(&self, path: &str) -> Option<PathGuard> {
		let mut set = self.0.lock().expect("active paths mutex poisoned");
		set.insert(path.to_string()).then(|| PathGuard {
			paths: self.0.clone(),
			path: path.to_string(),
		})
	}
}

/// Releases a claimed [`ActivePaths`] entry when dropped.
pub(crate) struct PathGuard {
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
	fn app_and_key() {
		assert_eq!(resolve_path("", "live", "cam0").as_deref(), Some("live/cam0"));
	}

	#[test]
	fn app_only() {
		assert_eq!(resolve_path("", "cam0", "").as_deref(), Some("cam0"));
	}

	#[test]
	fn prefix_is_prepended() {
		assert_eq!(resolve_path("live/", "cam0", "").as_deref(), Some("live/cam0"));
	}

	#[test]
	fn slashes_are_trimmed() {
		assert_eq!(resolve_path("", "/live/", "/cam0/").as_deref(), Some("live/cam0"));
	}

	#[test]
	fn empty_is_rejected() {
		assert_eq!(resolve_path("", "", ""), None);
	}

	#[test]
	fn active_paths_rejects_duplicates_and_releases_on_drop() {
		let active = ActivePaths::default();

		let guard = active.claim("live/cam0").expect("first claim succeeds");
		assert!(active.claim("live/cam0").is_none());
		let other = active.claim("live/cam1").expect("distinct path claims");

		drop(guard);
		assert!(active.claim("live/cam0").is_some());

		drop(other);
	}
}
