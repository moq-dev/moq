//! SRT listener and stream-id routing.
//!
//! SRT is a thin reliability/encryption layer over a UDP datagram stream whose
//! payload, by overwhelming convention, is MPEG-TS. We run an SRT listener and
//! route each incoming connection by its stream id, in one of two directions:
//!
//! - `m=publish` (the default): ingest. Pump the caller's TS payload through
//!   [`crate::ts::Publisher`] into the origin as a broadcast.
//! - `m=request`: egress. Re-mux the requested broadcast back to MPEG-TS with
//!   [`crate::ts::Subscriber`] and stream it to the caller, so VLC / ffmpeg can
//!   play `srt://host:port?streamid=#!::r=<broadcast>,m=request`.
//!
//! Routing: SRT's recommended stream-id form is `#!::r=<resource>,m=<mode>`. We
//! extract the `r=` resource when present and otherwise fall back to the raw
//! stream-id string; the `m=` mode picks the direction (absent / non-`request`
//! means ingest). The optional [`prefix`](Config::prefix) is prepended so a
//! single listener can namespace all of its streams (e.g. prefix `live/` +
//! stream id `cam0` -> broadcast `live/cam0`).
//!
//! Auth: this listener is currently unauthenticated. Anyone who can reach the
//! UDP port can publish or request any broadcast, so gate it with the host
//! firewall / a private network. SRT passphrase encryption and token checks are
//! the obvious next step (see `request.accept(Some(KeySettings))`).

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use moq_net::{OriginConsumer, OriginProducer};
use srt_tokio::SrtListener;
use srt_tokio::access::{
	AccessControlList, ConnectionMode, RejectReason, ServerRejectReason, StandardAccessControlEntry,
};
use srt_tokio::options::StreamId;

use crate::{Error, Result};

/// SRT payload size for egress: 7 MPEG-TS packets (7 x 188), the de-facto
/// standard for TS-over-SRT and a clean fit under the typical SRT MTU.
const SRT_PAYLOAD: usize = 7 * 188;

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
/// Stays pending forever (rather than resolving) when SRT is disabled, so it
/// composes cleanly inside a `tokio::select!` alongside a relay's other
/// long-running tasks.
pub async fn run(origin: OriginProducer, config: Config) -> Result<()> {
	let Some(listen) = config.listen else {
		tracing::info!("SRT gateway disabled (no listen address)");
		std::future::pending::<()>().await;
		unreachable!("pending future never resolves");
	};

	let (_listener, mut incoming) = SrtListener::builder().latency(config.latency).bind(listen).await?;
	tracing::info!(%listen, prefix = %config.prefix, "SRT listening");

	// Read side of the origin, used to serve `m=request` callers their broadcast.
	let consumer = origin.consume();

	// Tracks which broadcast paths are currently being ingested so a second
	// publisher on the same stream id is rejected (first-publisher-wins, like an
	// RTMP stream key) instead of being silently parked as a backup that could
	// take over the path when the first publisher drops.
	let active = ActivePaths::default();

	while let Some(request) = incoming.incoming().next().await {
		let remote = request.remote();
		let Some(route) = resolve_route(&config.prefix, request.stream_id()) else {
			tracing::warn!(%remote, stream_id = ?request.stream_id(), "rejecting SRT: no usable stream id");
			reject(request, ServerRejectReason::BadRequest, remote).await;
			continue;
		};

		// `m=request` reads a broadcast out; everything else publishes one in.
		if route.mode == ConnectionMode::Request {
			let path = route.path;
			let consumer = consumer.clone();
			tokio::spawn(async move {
				let socket = match request.accept(None).await {
					Ok(socket) => socket,
					Err(err) => {
						tracing::warn!(%remote, %err, "SRT accept failed");
						return;
					}
				};
				tracing::info!(%remote, %path, "SRT request accepted");
				if let Err(err) = serve_request(consumer, &path, socket).await {
					tracing::warn!(%remote, %path, %err, "SRT request ended with error");
				} else {
					tracing::info!(%remote, %path, "SRT request ended");
				}
			});
			continue;
		}

		// Claim the path before accepting; the guard releases it when the
		// connection task ends (success, error, or panic).
		let Some(guard) = active.claim(&route.path) else {
			tracing::warn!(%remote, path = %route.path, "rejecting SRT: path already being ingested");
			reject(request, ServerRejectReason::Forbidden, remote).await;
			continue;
		};

		let path = route.path;
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
			if let Err(err) = serve_publish(origin, &path, socket).await {
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

/// Pump one accepted SRT socket's MPEG-TS payload into the origin (`m=publish`).
async fn serve_publish(origin: OriginProducer, path: &str, mut socket: srt_tokio::SrtSocket) -> Result<()> {
	use futures::TryStreamExt;

	let mut publisher = crate::ts::Publisher::new(&origin, path)?;
	while let Some((_instant, bytes)) = socket.try_next().await? {
		publisher.feed(bytes)?;
	}
	publisher.finish()?;
	Ok(())
}

/// Mux the requested broadcast back to MPEG-TS and stream it to the SRT caller
/// (`m=request`).
///
/// Waits for the broadcast to be announced (so a caller may connect before the
/// publisher), then packs the muxer's output into [`SRT_PAYLOAD`]-sized SRT
/// messages. Returns once the broadcast ends or the caller disconnects.
async fn serve_request(origin: OriginConsumer, path: &str, mut socket: srt_tokio::SrtSocket) -> Result<()> {
	// Resolve the broadcast, but watch the socket while we wait: `announced_broadcast`
	// parks forever for a stream that is never published, and nothing else polls the
	// socket during that wait, so without this a caller who requests a non-existent
	// stream (or hangs up before it starts) would leak this task and its socket.
	let subscriber = tokio::select! {
		biased;
		_ = wait_closed(&mut socket) => {
			tracing::debug!(%path, "SRT request closed before its broadcast was available");
			return Ok(());
		}
		subscriber = crate::ts::Subscriber::new(&origin, path) => subscriber?,
	};

	let Some(mut subscriber) = subscriber else {
		tracing::warn!(%path, "SRT request for an unroutable broadcast");
		return Ok(());
	};

	// MPEG-TS is a continuous byte stream, so we coalesce the muxer's per-frame
	// output and slice it on a fixed boundary rather than preserving frames.
	//
	// Pace on the media clock: stamp each SRT payload with the media time of the
	// frame it carries, anchored to when the first frame went out. SRT's TSBPD
	// reconstructs that inter-frame spacing at the receiver, so a tune-in keyframe
	// burst is released at the media rate instead of all at once. (The Instant
	// passed to `send` is the packet's origin time feeding TSBPD, not a "send now"
	// instruction; `Instant::now()` would collapse the spacing into a burst.)
	//
	// We pace on the frame's presentation timestamp (the only clock the muxer
	// exposes) while frames transmit in decode order, so a B-frame stream's
	// per-GOP reorder leaves `send_at` slightly non-monotonic. That's harmless:
	// `saturating_sub` keeps it >= the anchor, and the receiver reorders from the
	// PTS/DTS carried inside the TS payload regardless.
	let anchor = Instant::now();
	let mut base = None;
	let mut send_at = anchor;
	let mut buffer = bytes::BytesMut::new();
	while let Some(frame) = subscriber.next().await? {
		let origin = *base.get_or_insert(frame.timestamp);
		send_at = anchor + Duration::from(frame.timestamp).saturating_sub(Duration::from(origin));

		buffer.extend_from_slice(&frame.payload);
		while buffer.len() >= SRT_PAYLOAD {
			socket.send((send_at, buffer.split_to(SRT_PAYLOAD).freeze())).await?;
		}
	}

	if !buffer.is_empty() {
		socket.send((send_at, buffer.freeze())).await?;
	}
	socket.close().await?;

	Ok(())
}

/// Resolve once the SRT caller hangs up (a clean close or an error), draining and
/// ignoring any unexpected inbound packets. A request caller normally sends
/// nothing, so this is purely a disconnect signal to race against the announce wait.
async fn wait_closed(socket: &mut srt_tokio::SrtSocket) {
	use futures::TryStreamExt;
	while let Ok(Some(_)) = socket.try_next().await {}
}

/// A routing decision derived from an SRT connection's stream id.
struct Route {
	/// Broadcast path (with [`Config::prefix`] applied) to publish into or read from.
	path: String,
	/// Direction: `Request` serves a broadcast out, anything else ingests one in.
	mode: ConnectionMode,
}

/// Derive a [`Route`] from an SRT stream id, applying `prefix`.
///
/// Prefers the standard `#!::r=<resource>,m=<mode>` form, then falls back to the
/// raw stream-id string (always treated as ingest). Returns `None` when there's
/// nothing usable to route on.
fn resolve_route(prefix: &str, stream_id: Option<&StreamId>) -> Option<Route> {
	let raw = stream_id?.as_str().trim();

	// Standard SRT access-control form: `#!::r=<resource>,m=<mode>,...`. Absent
	// `m=` defaults to publish, matching a bare stream id and OBS-style ingest.
	let mut resource = None;
	let mut mode = ConnectionMode::Publish;
	if let Ok(acl) = raw.parse::<AccessControlList>() {
		for entry in acl.0 {
			match StandardAccessControlEntry::try_from(entry) {
				Ok(StandardAccessControlEntry::ResourceName(name)) if !name.is_empty() => resource = Some(name),
				Ok(StandardAccessControlEntry::Mode(m)) => mode = m,
				_ => {}
			}
		}
	}

	// Fall back to the raw stream id (e.g. OBS-style `app/key`), but never to an
	// unparsed `#!::` control string.
	let name = match resource {
		Some(name) => name,
		None if raw.is_empty() || raw.starts_with("#!::") => return None,
		None => raw.to_string(),
	};

	Some(Route {
		path: format!("{prefix}{name}"),
		mode,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sid(s: &str) -> StreamId {
		StreamId::try_from(s.as_bytes().to_vec()).unwrap()
	}

	fn route(prefix: &str, s: &str) -> Option<Route> {
		resolve_route(prefix, Some(&sid(s)))
	}

	#[test]
	fn standard_resource_form() {
		let r = route("", "#!::r=live/cam0,m=publish").unwrap();
		assert_eq!(r.path, "live/cam0");
		assert_eq!(r.mode, ConnectionMode::Publish);
	}

	#[test]
	fn request_mode_is_egress() {
		let r = route("", "#!::r=live/cam0,m=request").unwrap();
		assert_eq!(r.path, "live/cam0");
		assert_eq!(r.mode, ConnectionMode::Request);
	}

	#[test]
	fn absent_mode_defaults_to_publish() {
		// Both a bare stream id and an `r=`-only ACL ingest by default.
		assert_eq!(route("", "app/key").unwrap().mode, ConnectionMode::Publish);
		assert_eq!(route("", "#!::r=cam0").unwrap().mode, ConnectionMode::Publish);
	}

	#[test]
	fn raw_stream_id() {
		let r = route("", "app/key").unwrap();
		assert_eq!(r.path, "app/key");
		assert_eq!(r.mode, ConnectionMode::Publish);
	}

	#[test]
	fn prefix_is_prepended() {
		assert_eq!(route("live/", "cam0").unwrap().path, "live/cam0");
		// Prefix applies to egress requests too.
		assert_eq!(route("live/", "#!::r=cam0,m=request").unwrap().path, "live/cam0");
	}

	#[test]
	fn missing_or_empty_is_rejected() {
		assert!(resolve_route("", None).is_none());
		assert!(route("", "").is_none());
		assert!(route("", "#!::").is_none());
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
