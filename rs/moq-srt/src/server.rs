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
use moq_net::origin;
use srt_tokio::access::{
	AccessControlList, ConnectionMode, RejectReason, ServerRejectReason, StandardAccessControlEntry,
};
use srt_tokio::options::{PacketCount, SocketOptions, StreamId};
use srt_tokio::{ConnectionRequest, SrtIncoming, SrtListener, SrtSocket};

use crate::Result;

/// Default SRT receive latency: the negotiated buffer that trades delay for loss
/// recovery. Override per-server with [`Server::bind`]'s `latency` argument.
pub(crate) const DEFAULT_LATENCY: Duration = Duration::from_millis(500);

/// SRT payload size for egress: 7 MPEG-TS packets (7 x 188), the de-facto
/// standard for TS-over-SRT and a clean fit under the typical SRT MTU.
const SRT_PAYLOAD: usize = 7 * 188;

/// Match libsrt's standard send-buffer window.
const SRT_BUFFER_PACKETS: PacketCount = PacketCount(8192);

/// srt-tokio defaults its sender to only 32 packets, so one large keyframe can
/// evict an unsent packet and wedge its send queue behind the missing sequence
/// number.
pub(crate) fn configure_buffers(options: &mut SocketOptions) {
	options.sender.buffer_size = SRT_BUFFER_PACKETS * options.session.max_segment_size;
}

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
	/// The negotiated SRT receive latency, reused as the egress skip threshold on
	/// each [`Subscribe`] (see [`crate::ts::Subscriber::new`]).
	latency: Duration,
}

impl Server {
	/// Bind an SRT listener on `addr` (SRT has no well-known port; 9000 is common).
	///
	/// `latency` is the SRT receive latency, negotiated at handshake time; pass
	/// `None` for a sensible default (500ms). It doubles as the egress skip
	/// threshold for [`Subscribe`] requests.
	pub async fn bind(addr: SocketAddr, latency: impl Into<Option<Duration>>) -> Result<Self> {
		let latency = latency.into().unwrap_or(DEFAULT_LATENCY);
		let (listener, incoming) = SrtListener::builder()
			.latency(latency)
			.set(configure_buffers)
			.bind(addr)
			.await?;
		Ok(Self {
			_listener: listener,
			incoming,
			latency,
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
				latency: self.latency,
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
	/// The SRT receive latency, reused as the egress skip threshold on a subscribe.
	latency: Duration,
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
	pub async fn accept(self, origin: &origin::Producer, path: impl moq_net::AsPath) -> Result<()> {
		let path = path.as_path();
		let socket = self.0.request.accept(None).await?;
		tracing::info!(peer = %self.0.peer, %path, "SRT publish accepted");
		serve_publish(origin, path.as_str(), socket).await
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
	pub async fn accept(self, origin: &origin::Consumer, path: impl moq_net::AsPath) -> Result<()> {
		let path = path.as_path();
		let socket = self.0.request.accept(None).await?;
		tracing::info!(peer = %self.0.peer, %path, "SRT subscribe accepted");
		serve_subscribe(origin, path.as_str(), socket, self.0.latency).await
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
pub(crate) async fn serve_publish(origin: &origin::Producer, path: &str, mut socket: SrtSocket) -> Result<()> {
	use futures::TryStreamExt;

	let mut publisher = crate::ts::Publisher::new(origin, path)?;

	// Run the read/feed loop so an error surfaces here instead of unwinding past
	// the publisher, which would drop it (and its tracks) with a bare Error::Dropped.
	let result: Result<()> = async {
		while let Some((_instant, bytes)) = socket.try_next().await? {
			publisher.feed(bytes)?;
		}
		Ok(())
	}
	.await;

	match &result {
		// Clean end (the caller closed): flush the final groups.
		Ok(()) => publisher.finish()?,
		// The socket or demux failed: abort with the real cause so subscribers see it.
		Err(err) => publisher.abort(moq_net::Error::Transport(err.to_string())),
	}
	result
}

/// Mux the requested broadcast back to MPEG-TS and stream it to the SRT caller
/// (`m=request`).
///
/// Waits for the broadcast to be announced (so a caller may connect before the
/// publisher), then packs the muxer's output into [`SRT_PAYLOAD`]-sized SRT
/// messages. Returns once the broadcast ends or the caller disconnects.
pub(crate) async fn serve_subscribe(
	origin: &origin::Consumer,
	path: &str,
	mut socket: SrtSocket,
	latency: Duration,
) -> Result<()> {
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
		subscriber = crate::ts::Subscriber::new(origin, path, latency) => subscriber?,
	};

	let Some(mut subscriber) = subscriber else {
		tracing::warn!(%path, "SRT subscribe for an unroutable broadcast");
		return Ok(());
	};

	// MPEG-TS is a continuous byte stream, so we coalesce the muxer's per-frame
	// output and slice it on a fixed boundary rather than preserving frames.
	//
	// Pace each payload on the media clock: the Instant handed to `send` is the
	// payload's origin time feeding the receiver's TSBPD, which reconstructs the
	// inter-frame spacing from it. We don't know the live playhead when a subscriber
	// attaches, so `pace` anchors it for us -- the newest frame is "now" and earlier
	// frames map to proportionally earlier instants, re-anchoring whenever the media
	// outruns wall-clock (a tune-in burst, a catch-up, or producer drift). `anchor`
	// and `base` are that media-clock anchor (`base`'s media time maps to `anchor`),
	// carried across frames and moved forward by `pace`.
	let mut anchor = Instant::now();
	let mut base = None;
	let mut send_at = anchor;
	let mut buffer = bytes::BytesMut::new();
	while let Some(frame) = subscriber.next().await? {
		// The media zero-point the rest pace against; `pace` re-anchors it forward to
		// the live edge whenever the media outruns wall-clock.
		let zero = *base.get_or_insert(frame.timestamp);
		let paced = pace(anchor, zero, frame.timestamp, Instant::now());
		send_at = paced.send_at;
		anchor = paced.anchor;
		base = Some(paced.base);

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

/// One frame's SRT send time plus the media-clock anchor to carry into the next
/// frame. `base`'s media time maps to `anchor` on the wall clock.
struct Paced {
	/// The Instant to stamp this payload with: its TSBPD origin time.
	send_at: Instant,
	anchor: Instant,
	base: moq_net::Timestamp,
}

/// Pace one SRT payload on the media clock, re-anchored to the live edge.
///
/// `send_at = anchor + (ts - base)` stamps the payload at its media time, so the
/// receiver's TSBPD reconstructs inter-frame spacing from it. But when that runs
/// ahead of `now` -- the media clock has outrun wall-clock: a subscriber tuning in
/// bursts the current GOP plus any catch-up backlog (frames whose media timestamps
/// span seconds, produced within milliseconds), or the producer drifts faster than
/// real time -- re-anchor the live edge to `now` (this frame becomes the playhead).
/// Otherwise the payload is stamped seconds into the future, where SRT holds it in
/// the sender past the receiver's TSBPD latency window and playback stalls after
/// ~one packet.
///
/// Re-anchoring only ever moves the anchor *forward*, so a frame that merely arrives
/// late -- network/CPU jitter, or a reordered B-frame whose PTS trails the edge --
/// keeps its earlier media instant instead of collapsing to its arrival instant, and
/// TSBPD still smooths the jitter into even playout. The cap is "never lead `now`":
/// the receiver owns the jitter buffer (the SRT latency parameter), so the sender
/// adds no lookahead of its own (a deliberate lead would just be `now + lead` here).
fn pace(anchor: Instant, base: moq_net::Timestamp, ts: moq_net::Timestamp, now: Instant) -> Paced {
	let send_at = match ts.checked_sub(base) {
		Ok(offset) => anchor + Duration::from(offset),
		// A reordered B-frame can carry a PTS before the anchor: pace it at that earlier
		// instant instead of collapsing it onto the anchor. `ts.checked_sub(base)` failed,
		// so `base >= ts` (same scale); fall back to the anchor if it can't be expressed.
		Err(_) => base
			.checked_sub(ts)
			.ok()
			.and_then(|behind| anchor.checked_sub(Duration::from(behind)))
			.unwrap_or(anchor),
	};
	if send_at > now {
		// Media outran wall-clock: re-anchor so this newest frame is the live edge.
		Paced {
			send_at: now,
			anchor: now,
			base: ts,
		}
	} else {
		Paced { send_at, anchor, base }
	}
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
	use bytes::Bytes;
	use std::net::SocketAddr;
	use std::time::Duration;

	#[test]
	fn send_buffer_uses_standard_srt_window() {
		let mut options = SocketOptions::default();
		configure_buffers(&mut options);

		assert_eq!(
			options.sender.buffer_size,
			SRT_BUFFER_PACKETS * options.session.max_segment_size
		);
	}

	#[tokio::test]
	async fn accepted_socket_sends_a_burst_larger_than_srt_tokio_default() {
		let probe = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
		let addr: SocketAddr = probe.local_addr().unwrap();
		drop(probe);

		let mut server = Server::bind(addr, None).await.unwrap();
		let caller = tokio::spawn(async move {
			SrtSocket::builder()
				.call(addr, Some("#!::r=buffer-test,m=request"))
				.await
				.unwrap()
		});

		let request = server.accept().await.expect("an SRT request");
		let Request::Subscribe(subscribe) = request else {
			panic!("m=request must create a subscribe request");
		};
		let mut sender = subscribe.0.request.accept(None).await.unwrap();
		let mut receiver = caller.await.unwrap();

		const MESSAGES: usize = 1024;
		for sequence in 0..MESSAGES {
			sender
				.send((Instant::now(), Bytes::copy_from_slice(&sequence.to_be_bytes())))
				.await
				.unwrap();
		}

		for sequence in 0..MESSAGES {
			let (_, payload) = tokio::time::timeout(Duration::from_secs(3), receiver.next())
				.await
				.expect("SRT sender stalled after its small default buffer overflowed")
				.expect("SRT sender closed before the burst finished")
				.unwrap();
			assert_eq!(payload.as_ref(), sequence.to_be_bytes());
		}
	}

	#[test]
	fn pace_re_anchors_to_live_edge() {
		use moq_net::Timestamp;
		use std::time::{Duration, Instant};
		let ms = |m: u64| Timestamp::from_micros(m * 1_000).unwrap();

		// Tune-in burst: the live edge (4132ms of media) is produced ~8ms after the
		// first frame (1400ms). It must re-anchor to `now` rather than stamp ~2.7s
		// into the future, which would stall the receiver's TSBPD after ~one payload.
		let start = Instant::now();
		let now = start + Duration::from_millis(8);
		let edge = pace(start, ms(1_400), ms(4_132), now);
		assert_eq!(edge.send_at, now, "live edge should pace to now");
		assert_eq!(edge.anchor, now);
		assert_eq!(
			edge.base.as_micros(),
			ms(4_132).as_micros(),
			"anchor moves up to the live edge"
		);

		// Part 1 moved the anchor to the live edge (now / 4132ms). A frame 33ms newer in
		// MEDIA that arrives 80ms later in WALL-clock (jitter) paces from that carried-
		// forward anchor -- its media instant (+33ms off the edge), not its 80ms arrival
		// instant -- so TSBPD still reconstructs smooth spacing.
		let jittered = pace(
			edge.anchor,
			edge.base,
			ms(4_165),
			edge.anchor + Duration::from_millis(80),
		);
		assert_eq!(
			jittered.send_at,
			edge.anchor + Duration::from_millis(33),
			"a late frame keeps its media instant, not its arrival instant"
		);
		assert_eq!(
			jittered.anchor, edge.anchor,
			"no re-anchor when media is behind wall-clock"
		);

		// A reordered B-frame can carry a PTS before the re-anchored live edge. Keep that
		// earlier media instant instead of flattening it onto the anchor.
		let reordered = pace(
			edge.anchor,
			edge.base,
			ms(4_099),
			edge.anchor + Duration::from_millis(20),
		);
		assert_eq!(
			reordered.send_at,
			edge.anchor - Duration::from_millis(33),
			"a reordered frame can pace before the anchor"
		);
		assert_eq!(reordered.anchor, edge.anchor, "no re-anchor when media trails the edge");
	}

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
