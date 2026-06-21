//! SRT server: accept connections, and hand each pending request to the caller
//! as a [`Request`] to authorize.
//!
//! [`Server::accept`] yields a [`Request`] for each incoming SRT connection,
//! before the handshake is finalized, classified by its stream-id `m=` mode into
//! one of two directions. The caller inspects [`Request::resource`] /
//! [`Request::stream_id`], makes an authorization decision, and either:
//!
//! - **[`Request::Publish`]**: [`Publish::accept`] (ingest the connection's
//!   MPEG-TS into an origin at a path) or [`Publish::reject`]. This is the
//!   contribution path (OBS, ffmpeg).
//! - **[`Request::Subscribe`]**: [`Subscribe::accept`] (re-mux a broadcast from
//!   an origin back to MPEG-TS and stream it down to the caller) or
//!   [`Subscribe::reject`]. This is the egress path: a player (VLC, ffmpeg) pulls
//!   `srt://host:port?streamid=#!::r=<broadcast>,m=request`.
//!
//! This mirrors `moq-native`'s `Server` / `Request`, so the gateway stays
//! unopinionated about auth: the embedder (e.g. a relay verifying the stream id
//! as a JWT) owns that policy. For the unauthenticated convenience that accepts
//! everything and routes by prefix, use [`crate::run`].

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use moq_net::{OriginConsumer, OriginProducer};
use srt_tokio::access::{
	AccessControlList, ConnectionMode, RejectReason, ServerRejectReason, StandardAccessControlEntry,
};
use srt_tokio::options::StreamId;
use srt_tokio::{ConnectionRequest, SrtIncoming, SrtListener, SrtSocket};

use crate::Result;

/// Default SRT receive latency: the negotiated buffer that trades delay for loss
/// recovery. Override per-server with [`Server::bind`]'s `latency` argument.
const DEFAULT_LATENCY: Duration = Duration::from_millis(200);

/// SRT payload size for egress: 7 MPEG-TS packets (7 x 188), the de-facto
/// standard for TS-over-SRT and a clean fit under the typical SRT MTU.
const SRT_PAYLOAD: usize = 7 * 188;

/// An SRT server that yields each incoming connection's pending request as a
/// [`Request`].
///
/// Build it with [`bind`](Self::bind), then loop on [`accept`](Self::accept).
/// Each [`Request`] is produced before the SRT handshake is finalized, so the
/// caller can authorize (and pick the broadcast path) before any media flows.
pub struct Server {
	/// Held to keep the listener (and its UDP socket) alive for the server's lifetime.
	_listener: SrtListener,
	incoming: SrtIncoming,
}

impl Server {
	/// Bind an SRT listener on `addr` (SRT has no well-known port; 9000 is common).
	///
	/// `latency` is the SRT receive latency, negotiated at handshake time; pass
	/// `None` for a sensible default (200ms).
	pub async fn bind(addr: SocketAddr, latency: impl Into<Option<Duration>>) -> Result<Self> {
		let latency = latency.into().unwrap_or(DEFAULT_LATENCY);
		let (listener, incoming) = SrtListener::builder().latency(latency).bind(addr).await?;
		Ok(Self {
			_listener: listener,
			incoming,
		})
	}

	/// Wait for the next connection that wants to publish or subscribe.
	///
	/// Connections whose stream id can't be routed (no usable resource name) are
	/// rejected internally and skipped, so every [`Request`] returned is
	/// actionable. Returns `None` only if the listener stops accepting (it
	/// currently never does).
	pub async fn accept(&mut self) -> Option<Request> {
		while let Some(request) = self.incoming.incoming().next().await {
			let peer = request.remote();
			let Some((resource, mode)) = parse_stream_id(request.stream_id()) else {
				tracing::warn!(%peer, stream_id = ?request.stream_id(), "rejecting SRT: no usable stream id");
				reject_log(request, ServerRejectReason::BadRequest, peer).await;
				continue;
			};

			let stream_id = request.stream_id().map(|id| id.as_str().to_string());
			let pending = Pending {
				request,
				resource,
				stream_id,
				peer,
			};

			// `m=request` reads a broadcast out; everything else publishes one in.
			return Some(match mode {
				ConnectionMode::Request => Request::Subscribe(Subscribe(pending)),
				_ => Request::Publish(Publish(pending)),
			});
		}

		None
	}
}

/// Common state behind a pending [`Request`]: the SRT connection plus the
/// routing info parsed from its stream id.
struct Pending {
	request: ConnectionRequest,
	/// The resource name to route on: the stream id's `r=` value, or the raw
	/// stream id when it carries no access-control list.
	resource: String,
	/// The raw stream id string, if any. Exposed so an embedder can parse its own
	/// fields out of it (e.g. a token in `u=` or a custom key).
	stream_id: Option<String>,
	peer: SocketAddr,
}

/// What an accepted SRT connection wants: to contribute media ([`Publish`]) or to
/// view it ([`Subscribe`]).
///
/// Yielded by [`Server::accept`], classified by the stream id's `m=` mode.
/// Inspect [`resource`](Self::resource) / [`stream_id`](Self::stream_id), then
/// match to authorize the right direction. Dropping it without accepting or
/// rejecting drops the connection.
#[non_exhaustive]
pub enum Request {
	/// A client pushing media in (OBS, ffmpeg). Ingest it with [`Publish::accept`].
	Publish(Publish),
	/// A client pulling media out (VLC, ffmpeg). Serve it with [`Subscribe::accept`].
	Subscribe(Subscribe),
}

impl Request {
	/// The resource name to route on: the stream id's `r=` value, or the raw
	/// stream id when it carries no access-control list.
	pub fn resource(&self) -> &str {
		match self {
			Request::Publish(r) => r.resource(),
			Request::Subscribe(r) => r.resource(),
		}
	}

	/// The raw SRT stream id, if the client supplied one.
	pub fn stream_id(&self) -> Option<&str> {
		match self {
			Request::Publish(r) => r.stream_id(),
			Request::Subscribe(r) => r.stream_id(),
		}
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		match self {
			Request::Publish(r) => r.peer(),
			Request::Subscribe(r) => r.peer(),
		}
	}
}

/// A pending SRT publish (contribution), waiting on the caller to authorize it.
///
/// Inspect [`resource`](Self::resource) / [`stream_id`](Self::stream_id), then
/// either [`accept`](Self::accept) the publish into an origin at a chosen
/// broadcast path or [`reject`](Self::reject) it. Dropping it without either
/// drops the connection.
pub struct Publish(Pending);

impl Publish {
	/// The resource name to route on (the stream id's `r=` value, or the raw
	/// stream id).
	pub fn resource(&self) -> &str {
		&self.0.resource
	}

	/// The raw SRT stream id, if the client supplied one.
	///
	/// Conventionally just a resource path, but an embedder can treat it (or a
	/// field within it) as a token to authenticate the publish.
	pub fn stream_id(&self) -> Option<&str> {
		self.0.stream_id.as_deref()
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		self.0.peer
	}

	/// Accept the publish: announce a broadcast at `path` in `origin` and pump the
	/// connection's MPEG-TS into it until the client disconnects.
	///
	/// `origin` is whatever the caller wants the media published into (e.g. a
	/// relay's shared origin, optionally scoped per the authenticated token). This
	/// future resolves when the connection ends, so callers usually run it on its
	/// own task.
	pub async fn accept(self, origin: &OriginProducer, path: &str) -> Result<()> {
		let socket = self.0.request.accept(None).await?;
		tracing::info!(peer = %self.0.peer, %path, "SRT publish accepted");
		serve_publish(origin, path, socket).await
	}

	/// Reject the publish, sending the client a `Forbidden` rejection.
	pub async fn reject(self) -> Result<()> {
		Ok(self
			.0
			.request
			.reject(RejectReason::Server(ServerRejectReason::Forbidden))
			.await?)
	}
}

/// A pending SRT subscribe (egress), waiting on the caller to authorize it.
///
/// The viewing counterpart of [`Publish`]: inspect [`resource`](Self::resource) /
/// [`stream_id`](Self::stream_id), then [`accept`](Self::accept) to serve a
/// broadcast from an origin down to the caller, or [`reject`](Self::reject) it.
/// Dropping it without either drops the connection.
pub struct Subscribe(Pending);

impl Subscribe {
	/// The resource name to route on (the stream id's `r=` value, or the raw
	/// stream id).
	pub fn resource(&self) -> &str {
		&self.0.resource
	}

	/// The raw SRT stream id, if the client supplied one.
	///
	/// As with a publish, an embedder can treat this as a token to authorize the
	/// viewer.
	pub fn stream_id(&self) -> Option<&str> {
		self.0.stream_id.as_deref()
	}

	/// The remote peer address.
	pub fn peer(&self) -> SocketAddr {
		self.0.peer
	}

	/// Accept the subscribe: resolve the broadcast at `path` in `origin`, re-mux
	/// it to MPEG-TS, and stream it down to the caller until either side ends.
	///
	/// Waits for the broadcast to be announced (so a caller may connect before the
	/// publisher), cancelling cleanly if the caller disconnects first. This future
	/// resolves when playback ends, so callers usually run it on its own task.
	pub async fn accept(self, origin: &OriginConsumer, path: &str) -> Result<()> {
		let socket = self.0.request.accept(None).await?;
		tracing::info!(peer = %self.0.peer, %path, "SRT subscribe accepted");
		serve_subscribe(origin, path, socket).await
	}

	/// Reject the subscribe, sending the client a `Forbidden` rejection.
	pub async fn reject(self) -> Result<()> {
		Ok(self
			.0
			.request
			.reject(RejectReason::Server(ServerRejectReason::Forbidden))
			.await?)
	}
}

/// Reject a connection request, logging (but not propagating) a send failure.
/// Used for connections the server drops itself, before they reach the caller.
async fn reject_log(request: ConnectionRequest, reason: ServerRejectReason, peer: SocketAddr) {
	if let Err(err) = request.reject(RejectReason::Server(reason)).await {
		tracing::debug!(%peer, %err, "failed to send SRT rejection");
	}
}

/// Pump one accepted SRT socket's MPEG-TS payload into the origin (`m=publish`).
async fn serve_publish(origin: &OriginProducer, path: &str, mut socket: SrtSocket) -> Result<()> {
	use futures::TryStreamExt;

	let mut publisher = crate::ts::Publisher::new(origin, path)?;
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
async fn serve_subscribe(origin: &OriginConsumer, path: &str, mut socket: SrtSocket) -> Result<()> {
	// Resolve the broadcast, but watch the socket while we wait: `announced_broadcast`
	// parks forever for a stream that is never published, and nothing else polls the
	// socket during that wait, so without this a caller who requests a non-existent
	// stream (or hangs up before it starts) would leak this task and its socket.
	let subscriber = tokio::select! {
		biased;
		_ = wait_closed(&mut socket) => {
			tracing::debug!(%path, "SRT subscribe closed before its broadcast was available");
			return Ok(());
		}
		subscriber = crate::ts::Subscriber::new(origin, path) => subscriber?,
	};

	let Some(mut subscriber) = subscriber else {
		tracing::warn!(%path, "SRT subscribe for an unroutable broadcast");
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
/// ignoring any unexpected inbound packets. A subscribe caller normally sends
/// nothing, so this is purely a disconnect signal to race against the announce wait.
async fn wait_closed(socket: &mut SrtSocket) {
	use futures::TryStreamExt;
	while let Ok(Some(_)) = socket.try_next().await {}
}

/// Parse an SRT stream id into its resource name and connection mode.
///
/// Prefers the standard `#!::r=<resource>,m=<mode>` form, then falls back to the
/// raw stream-id string (always treated as publish). Returns `None` when there's
/// nothing usable to route on.
fn parse_stream_id(stream_id: Option<&StreamId>) -> Option<(String, ConnectionMode)> {
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

	Some((name, mode))
}

#[cfg(test)]
mod tests {
	use super::*;

	fn sid(s: &str) -> StreamId {
		StreamId::try_from(s.as_bytes().to_vec()).unwrap()
	}

	fn parse(s: &str) -> Option<(String, ConnectionMode)> {
		parse_stream_id(Some(&sid(s)))
	}

	#[test]
	fn standard_resource_form() {
		let (resource, mode) = parse("#!::r=live/cam0,m=publish").unwrap();
		assert_eq!(resource, "live/cam0");
		assert_eq!(mode, ConnectionMode::Publish);
	}

	#[test]
	fn request_mode_is_egress() {
		let (resource, mode) = parse("#!::r=live/cam0,m=request").unwrap();
		assert_eq!(resource, "live/cam0");
		assert_eq!(mode, ConnectionMode::Request);
	}

	#[test]
	fn absent_mode_defaults_to_publish() {
		// Both a bare stream id and an `r=`-only ACL ingest by default.
		assert_eq!(parse("app/key").unwrap().1, ConnectionMode::Publish);
		assert_eq!(parse("#!::r=cam0").unwrap().1, ConnectionMode::Publish);
	}

	#[test]
	fn raw_stream_id() {
		let (resource, mode) = parse("app/key").unwrap();
		assert_eq!(resource, "app/key");
		assert_eq!(mode, ConnectionMode::Publish);
	}

	#[test]
	fn missing_or_empty_is_rejected() {
		assert!(parse_stream_id(None).is_none());
		assert!(parse("").is_none());
		assert!(parse("#!::").is_none());
	}
}
