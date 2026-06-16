//! SRT listener and stream-id routing.
//!
//! SRT is a thin reliability/encryption layer over a UDP datagram stream whose
//! payload, by overwhelming convention, is MPEG-TS. We run an SRT listener,
//! route each incoming connection to a broadcast path derived from its stream
//! id, and pump the TS payload through [`crate::ts::Publisher`] into the origin.
//!
//! Routing: SRT's recommended stream-id form is `#!::r=<resource>,m=publish`.
//! We extract the `r=` resource when present and otherwise fall back to the raw
//! stream-id string. The optional [`prefix`](Config::prefix) is prepended so a
//! single listener can namespace all of its ingests (e.g. prefix `live/` +
//! stream id `cam0` -> broadcast `live/cam0`).
//!
//! Auth: this listener is currently unauthenticated. Anyone who can reach the
//! UDP port can publish, so gate it with the host firewall / a private network.
//! SRT passphrase encryption and token checks are the obvious next step (see
//! `request.accept(Some(KeySettings))`).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use moq_net::OriginProducer;
use srt_tokio::SrtListener;
use srt_tokio::access::{AccessControlList, RejectReason, ServerRejectReason, StandardAccessControlEntry};
use srt_tokio::options::StreamId;

use crate::{Error, Result};

/// SRT ingest configuration.
///
/// Construct via [`Config::default`] and set the fields you need, so new
/// options stay additive. Ingest is disabled (and [`run`] stays pending) unless
/// [`listen`](Config::listen) is set, letting an embedding relay run without SRT
/// until it's configured.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
	/// Address to listen on for SRT ingest (e.g. `0.0.0.0:9000`). When `None`,
	/// SRT ingest is disabled.
	pub listen: Option<SocketAddr>,

	/// Prefix prepended to every ingested broadcast path. Lets one listener
	/// namespace all of its streams (e.g. `live/`).
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

/// Run the SRT ingest listener until it fails, publishing each connection into
/// `origin` as a broadcast.
///
/// Stays pending forever (rather than resolving) when SRT is disabled, so it
/// composes cleanly inside a `tokio::select!` alongside a relay's other
/// long-running tasks.
pub async fn run(origin: OriginProducer, config: Config) -> Result<()> {
	let Some(listen) = config.listen else {
		tracing::info!("SRT ingest disabled (no listen address)");
		std::future::pending::<()>().await;
		unreachable!("pending future never resolves");
	};

	let (_listener, mut incoming) = SrtListener::builder().latency(config.latency).bind(listen).await?;
	tracing::info!(%listen, prefix = %config.prefix, "SRT ingest listening");

	// Tracks which broadcast paths are currently being ingested so a second
	// publisher on the same stream id is rejected (first-publisher-wins, like an
	// RTMP stream key) instead of being silently parked as a backup that could
	// take over the path when the first publisher drops.
	let active = ActivePaths::default();

	while let Some(request) = incoming.incoming().next().await {
		let remote = request.remote();
		let Some(path) = resolve_path(&config.prefix, request.stream_id()) else {
			tracing::warn!(%remote, stream_id = ?request.stream_id(), "rejecting SRT: no usable stream id");
			reject(request, ServerRejectReason::BadRequest, remote).await;
			continue;
		};

		// Claim the path before accepting; the guard releases it when the
		// connection task ends (success, error, or panic).
		let Some(guard) = active.claim(&path) else {
			tracing::warn!(%remote, %path, "rejecting SRT: path already being ingested");
			reject(request, ServerRejectReason::Forbidden, remote).await;
			continue;
		};

		let origin = origin.clone();
		tokio::spawn(async move {
			let _guard = guard;
			let socket = match request.accept(None).await {
				Ok(socket) => socket,
				Err(err) => {
					tracing::warn!(%remote, %err, "SRT accept failed");
					return;
				}
			};
			tracing::info!(%remote, %path, "SRT connection accepted");
			if let Err(err) = serve(origin, &path, socket).await {
				tracing::warn!(%remote, %path, %err, "SRT ingest ended with error");
			} else {
				tracing::info!(%remote, %path, "SRT ingest ended");
			}
		});
	}

	Err(Error::from(anyhow::anyhow!(
		"SRT listener stopped accepting connections"
	)))
}

/// Reject a connection request, logging (but not propagating) a send failure.
async fn reject(request: srt_tokio::ConnectionRequest, reason: ServerRejectReason, remote: SocketAddr) {
	if let Err(err) = request.reject(RejectReason::Server(reason)).await {
		tracing::debug!(%remote, %err, "failed to send SRT rejection");
	}
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

/// Pump one accepted SRT socket's MPEG-TS payload into the origin.
async fn serve(origin: OriginProducer, path: &str, mut socket: srt_tokio::SrtSocket) -> Result<()> {
	use futures::TryStreamExt;

	let mut publisher = crate::ts::Publisher::new(&origin, path)?;
	while let Some((_instant, bytes)) = socket.try_next().await? {
		publisher.feed(bytes)?;
	}
	publisher.finish()?;
	Ok(())
}

/// Derive a broadcast path from an SRT stream id, applying `prefix`.
///
/// Prefers the standard `#!::r=<resource>` form, then falls back to the raw
/// stream-id string. Returns `None` when there's nothing usable to route on.
fn resolve_path(prefix: &str, stream_id: Option<&StreamId>) -> Option<String> {
	let raw = stream_id?.as_str().trim();

	// Standard SRT access-control form: `#!::r=<resource>,m=publish,...`.
	let resource = raw.parse::<AccessControlList>().ok().and_then(|acl| {
		acl.0
			.into_iter()
			.find_map(|entry| match StandardAccessControlEntry::try_from(entry) {
				Ok(StandardAccessControlEntry::ResourceName(name)) if !name.is_empty() => Some(name),
				_ => None,
			})
	});

	// Fall back to the raw stream id (e.g. OBS-style `app/key`), but never to an
	// unparsed `#!::` control string.
	let name = match resource {
		Some(name) => name,
		None if raw.is_empty() || raw.starts_with("#!::") => return None,
		None => raw.to_string(),
	};

	Some(format!("{prefix}{name}"))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sid(s: &str) -> StreamId {
		StreamId::try_from(s.as_bytes().to_vec()).unwrap()
	}

	#[test]
	fn standard_resource_form() {
		assert_eq!(
			resolve_path("", Some(&sid("#!::r=live/cam0,m=publish"))).as_deref(),
			Some("live/cam0")
		);
	}

	#[test]
	fn raw_stream_id() {
		assert_eq!(resolve_path("", Some(&sid("app/key"))).as_deref(), Some("app/key"));
	}

	#[test]
	fn prefix_is_prepended() {
		assert_eq!(resolve_path("live/", Some(&sid("cam0"))).as_deref(), Some("live/cam0"));
	}

	#[test]
	fn missing_or_empty_is_rejected() {
		assert_eq!(resolve_path("", None), None);
		assert_eq!(resolve_path("", Some(&sid(""))), None);
		assert_eq!(resolve_path("", Some(&sid("#!::"))), None);
	}

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
