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
use moq_mux::container::Frame;
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

/// Coalesce TS bytes that share one SRT pacing instant.
#[derive(Default)]
struct SrtChunker {
	buffer: bytes::BytesMut,
	send_at: Option<Instant>,
}

impl SrtChunker {
	/// Add one muxer frame, flushing a partial chunk before its pacing instant changes.
	fn push(&mut self, send_at: Instant, payload: &[u8]) -> Vec<(Instant, bytes::Bytes)> {
		if payload.is_empty() {
			return Vec::new();
		}

		let mut chunks = Vec::new();
		if self.send_at.is_some_and(|buffered_at| buffered_at != send_at) {
			chunks.extend(self.flush());
		}

		self.send_at = Some(send_at);
		self.buffer.extend_from_slice(payload);
		while self.buffer.len() >= SRT_PAYLOAD {
			chunks.push((send_at, self.buffer.split_to(SRT_PAYLOAD).freeze()));
		}

		if self.buffer.is_empty() {
			self.send_at = None;
		}
		chunks
	}

	/// Flush the final partial payload, if any.
	fn flush(&mut self) -> Option<(Instant, bytes::Bytes)> {
		let send_at = self.send_at.take()?;
		debug_assert!(!self.buffer.is_empty());
		Some((send_at, self.buffer.split().freeze()))
	}
}

/// Turns the muxer's frames into SRT payloads stamped with the instant each
/// should be transmitted at.
///
/// MPEG-TS is a continuous byte stream, so bytes that share a pacing instant are
/// coalesced and sliced on a fixed boundary. A partial payload is flushed before
/// the instant changes: one SRT message has only one TSBPD timestamp, so mixing
/// frames would re-stamp the earlier bytes with the frame that completed the chunk.
///
/// Each payload is paced on the media clock: the [`Instant`] handed to `send` is
/// the payload's origin time feeding the receiver's TSBPD, which reconstructs the
/// inter-frame spacing from it. We don't know the live playhead when a subscriber
/// attaches, so the pacer anchors it for us -- the newest frame is "now" and
/// earlier frames map to proportionally earlier instants, re-anchoring whenever
/// the media outruns wall-clock (a tune-in burst, a catch-up, or producer drift).
/// The default zero lead is deliberate: the receiver owns the jitter buffer (the
/// SRT latency parameter), so the sender adds no lookahead of its own.
#[derive(Default)]
struct Egress {
	pacer: moq_mux::Pacer,
	/// The first payload's send instant, the floor every later one is clamped up to
	/// (see [`clamp_to_floor`]).
	floor: Option<Instant>,
	chunker: SrtChunker,
	/// The muxer generation the pacer's anchor belongs to.
	discontinuity: u64,
}

impl Egress {
	/// Pace one muxer frame, returning the SRT payloads it completed.
	///
	/// `discontinuity` is the muxer's counter for `frame`. A change means the
	/// publisher rewound and the muxer restarted its program clock, so the anchor
	/// this pacer holds now belongs to a timeline that no longer exists: mapping the
	/// new generation through it puts every frame of the rewound span before the
	/// first packet, where the floor collapses all of them onto one instant and the
	/// receiver sees the program stop until the media climbs back. Make the frame
	/// the live edge instead, and pace the rest of the generation off it.
	fn push(&mut self, frame: &Frame, discontinuity: u64, now: Instant) -> Vec<(Instant, bytes::Bytes)> {
		if discontinuity == self.discontinuity {
			let send_at = clamp_to_floor(self.pacer.pace(frame.timestamp, now), &mut self.floor);
			return self.chunker.push(send_at, &frame.payload);
		}

		self.discontinuity = discontinuity;
		// The old generation's tail keeps the instant it was paced at: it is media that
		// already happened, and the new generation's stamp would claim otherwise.
		let mut chunks = Vec::from_iter(self.chunker.flush());
		let send_at = clamp_to_floor(self.pacer.hurry(frame.timestamp, now), &mut self.floor);
		chunks.extend(self.chunker.push(send_at, &frame.payload));
		chunks
	}

	/// Flush the final partial payload, if any.
	fn flush(&mut self) -> Option<(Instant, bytes::Bytes)> {
		self.chunker.flush()
	}
}

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

	let mut egress = Egress::default();
	while let Some(frame) = subscriber.next().await? {
		// Sample the muxer's generation alongside the frame it describes, so a
		// publisher rewind re-anchors the pacing rather than mapping the new timeline
		// through the old anchor.
		for chunk in egress.push(&frame, subscriber.discontinuity(), Instant::now()) {
			socket.send(chunk).await?;
		}
	}

	if let Some(chunk) = egress.flush() {
		socket.send(chunk).await?;
	}
	socket.close().await?;

	Ok(())
}

/// Clamp a paced send instant up to the connection's first one, seeding that floor
/// on the first call.
///
/// The receiver anchors its TSBPD clock on the first packet, and an SRT packet
/// timestamp is `u32` microseconds relative to the socket epoch, so a later payload
/// stamped *before* the first underflows on the receiver -- it wraps ~4295s into the
/// future, and in-order TSBPD delivery stalls behind it after ~one packet. `pace`
/// paces a reordered B-frame *before* the current anchor, and at tune-in the anchor
/// sits at the first packet, so without this clamp that reorder underflows.
fn clamp_to_floor(send_at: Instant, floor: &mut Option<Instant>) -> Instant {
	send_at.max(*floor.get_or_insert(send_at))
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

	/// Regression for #2978: a frame that completes a chunk must not re-stamp bytes
	/// already buffered from an earlier pacing instant.
	#[test]
	fn chunker_flushes_before_the_pacing_instant_changes() {
		let first = Instant::now();
		let second = first + Duration::from_millis(25);
		let mut chunker = SrtChunker::default();

		assert!(chunker.push(first, &[1; 188]).is_empty());
		let flushed = chunker.push(second, &[2; SRT_PAYLOAD - 188]);
		assert_eq!(flushed.len(), 1);
		assert_eq!(flushed[0].0, first);
		assert_eq!(flushed[0].1.as_ref(), &[1; 188]);

		let completed = chunker.push(second, &[3; 188]);
		assert_eq!(completed.len(), 1);
		assert_eq!(completed[0].0, second);
		assert_eq!(&completed[0].1[..SRT_PAYLOAD - 188], &[2; SRT_PAYLOAD - 188]);
		assert_eq!(&completed[0].1[SRT_PAYLOAD - 188..], &[3; 188]);
		assert!(chunker.flush().is_none());
	}

	/// Regression: srt-tokio's 32-packet default sender buffer evicts unsent packets
	/// once a burst overflows it, wedging the connection within the first few messages
	/// (see [`configure_buffers`]).
	#[tokio::test]
	async fn accepted_socket_sends_a_burst_larger_than_srt_tokio_default() {
		let probe = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
		let addr: SocketAddr = probe.local_addr().unwrap();
		drop(probe);

		// TSBPD holds every payload for the negotiated latency before releasing it, so
		// ask for a short one: this asserts buffering, not delay.
		let mut server = Server::bind(addr, Duration::from_millis(50)).await.unwrap();
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

		// Several times srt-tokio's 32-packet default, which stalls before the tenth
		// message. Keep it well under the ~1000 packets the sender paces out per second
		// while its send buffer ages: past that, SRT drops the tail of the burst as too
		// late and (silently, in srt-tokio) never retransmits it, which is a property of
		// the burst size rather than of the buffer under test.
		const MESSAGES: usize = 128;
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

	/// One muxer frame of `payload` bytes at `micros` of media time.
	fn frame(micros: u64, payload: &[u8]) -> Frame {
		Frame {
			timestamp: moq_net::Timestamp::from_micros(micros).unwrap(),
			duration: None,
			payload: Bytes::copy_from_slice(payload),
			keyframe: false,
		}
	}

	/// Regression: a reordered frame paced before the first packet must be clamped up
	/// to it, not transmitted with an earlier SRT timestamp. Reproduces the tune-in
	/// sequence a looping TS source triggers intermittently: a fast first delivery
	/// leaves the anchor at the live edge, then a newer frame re-anchors and an older
	/// (reordered) frame paces behind it -- below the first packet, which would
	/// underflow the receiver's u32 timestamp and stall in-order delivery after ~one
	/// packet.
	#[test]
	fn reordered_frame_is_clamped_to_the_first_packet() {
		use moq_net::Timestamp;
		let ms = |m: u64| Timestamp::from_micros(m * 1_000).unwrap();

		// Drive pace + clamp exactly like `serve_subscribe`, with controlled `now`s so
		// the second frame re-anchors (its media outruns wall-clock) and the third is a
		// reorder whose media trails the new anchor.
		let start = Instant::now();
		let mut pacer = moq_mux::Pacer::default();
		let mut floor = None;

		// i0: first frame, delivered ~instantly -> stamped at the live edge.
		let first = clamp_to_floor(pacer.pace(ms(1_400), start), &mut floor);
		// i1: 83ms newer in media, produced ~1ms later -> re-anchors to `now`.
		let _ = clamp_to_floor(pacer.pace(ms(1_483), start + Duration::from_millis(1)), &mut floor);
		// i2: a reordered B-frame 41ms behind the new anchor.
		let unclamped = pacer.pace(ms(1_442), start + Duration::from_millis(2));
		let clamped = clamp_to_floor(unclamped, &mut floor);

		assert!(
			unclamped < first,
			"the reorder paces before the first packet without the clamp (the bug)"
		);
		assert_eq!(clamped, first, "the clamp holds it at the first packet's instant");
	}

	/// A reordered (B-frame) timestamp arrives on the same generation, so it must be
	/// clamped to the first packet rather than re-anchored: treating every backwards
	/// step as a rewind would drag the whole stream onto the reorder's instant.
	#[test]
	fn a_reorder_is_not_a_rewind() {
		let start = Instant::now();
		let mut egress = Egress::default();

		let first = egress.push(&frame(1_400_000, &[1; SRT_PAYLOAD]), 0, start);
		let edge = egress.push(
			&frame(1_483_000, &[2; SRT_PAYLOAD]),
			0,
			start + Duration::from_millis(1),
		);
		let reorder = egress.push(
			&frame(1_442_000, &[3; SRT_PAYLOAD]),
			0,
			start + Duration::from_millis(2),
		);

		assert_eq!(first[0].0, start, "the first payload anchors the connection");
		assert_eq!(edge[0].0, start + Duration::from_millis(1), "the live edge re-anchors");
		assert_eq!(reorder[0].0, first[0].0, "the reorder is clamped, not re-anchored");
	}

	/// The 33-bit PTS wrap lives in the TS packets the muxer writes, not in the media
	/// timestamps it stamps its frames with, so it never bumps the generation counter
	/// and pacing runs straight through it.
	#[test]
	fn the_33_bit_wrap_is_not_a_rewind() {
		// 2^33 ticks of the 90 kHz TS clock, in microseconds: where a PTS wraps.
		const WRAP: u64 = (1u64 << 33) * 100 / 9;
		let start = Instant::now();
		let mut egress = Egress::default();

		let before = egress.push(&frame(WRAP - 25_000, &[1; SRT_PAYLOAD]), 0, start);
		let after = egress.push(
			&frame(WRAP + 25_000, &[2; SRT_PAYLOAD]),
			0,
			start + Duration::from_millis(50),
		);

		assert_eq!(before[0].0, start);
		assert_eq!(
			after[0].0,
			start + Duration::from_millis(50),
			"the wrap paces like any other 50ms step"
		);
	}

	/// Regression for the SRT half of #2833: the muxer restarts its program clock on a
	/// publisher rewind, so an egress that keeps its old anchor maps the whole rewound
	/// span into the past, where the first-packet floor collapses it onto one instant.
	/// The receiver then sees the program stop until the media climbs back to where it
	/// left off, then the accumulated backlog arrive at once.
	#[test]
	fn a_rewind_re_anchors_the_pacing() {
		const SLOT: Duration = Duration::from_millis(25);
		let start = Instant::now();
		let mut egress = Egress::default();

		// Ten minutes into the program, on the muxer's 25ms grid.
		let first = egress.push(&frame(600_000_000, &[1; SRT_PAYLOAD]), 0, start);
		let second = egress.push(&frame(600_025_000, &[1; SRT_PAYLOAD]), 0, start + SLOT);
		assert_eq!(first[0].0, start, "the first payload anchors the connection");
		assert_eq!(second[0].0, start + SLOT);

		// The publisher rewinds to the top of its source.
		let mut now = start + 2 * SLOT;
		let rewound = egress.push(&frame(0, &[2; SRT_PAYLOAD]), 1, now);
		assert_eq!(rewound[0].0, now, "the new generation becomes the live edge");

		// The grid slots that follow pace off the new anchor. Against the old one every
		// one of them is ten minutes in the past and clamps to `start`, so the receiver
		// gets the next ten minutes of program under a single timestamp.
		for slot in 1..=8u64 {
			now += SLOT;
			let chunks = egress.push(&frame(slot * 25_000, &[3; SRT_PAYLOAD]), 1, now);
			assert_eq!(chunks[0].0, now, "slot {slot} paces off the new anchor");
		}
	}

	/// The old generation's buffered tail goes out under the instant it was paced at.
	/// One SRT message carries one TSBPD timestamp, and the chunker only splits when
	/// the instant changes, so the flush has to be explicit: a rewind paced within the
	/// same clock tick would otherwise fold media that already played into the new
	/// program's first payload.
	#[test]
	fn a_rewind_flushes_the_partial_chunk_first() {
		const SLOT: Duration = Duration::from_millis(25);
		let start = Instant::now();
		let mut egress = Egress::default();

		// A partial chunk buffered under the first generation, then a rewind paced at
		// the very same instant.
		assert!(egress.push(&frame(600_000_000, &[1; 188]), 0, start).is_empty());
		let chunks = egress.push(&frame(0, &[2; SRT_PAYLOAD]), 1, start);
		assert_eq!(chunks.len(), 2, "the old tail is its own payload");
		assert_eq!(chunks[0].1.as_ref(), [1u8; 188].as_slice());
		assert_eq!(chunks[1].1.as_ref(), [2u8; SRT_PAYLOAD].as_slice());

		// And a tail paced earlier keeps that earlier instant rather than the rewind's.
		assert!(egress.push(&frame(25_000, &[3; 188]), 1, start + SLOT).is_empty());
		let chunks = egress.push(&frame(0, &[4; SRT_PAYLOAD]), 2, start + 2 * SLOT);
		assert_eq!(chunks[0].0, start + SLOT, "the old tail keeps its own instant");
		assert_eq!(chunks[0].1.as_ref(), [3u8; 188].as_slice());
		assert_eq!(chunks[1].0, start + 2 * SLOT, "the new generation is the live edge");
	}

	/// End to end over a real SRT receiver: a publisher that rewinds ten minutes must
	/// keep the program flowing, with the new generation stamped at the wall clock it
	/// was muxed at rather than back at the connection's first packet.
	///
	/// The receiver's TSBPD releases each message at the origin instant the sender
	/// stamped, so what it observes is exactly the pacing decision under test: with a
	/// stale anchor the new generation carries the first packet's timestamp, which is
	/// already a latency in the past by the time it is sent, so it is released at once
	/// (or dropped as too late) instead of on the media clock.
	#[tokio::test]
	async fn a_publisher_rewind_keeps_an_srt_receiver_playing() {
		use moq_mux::catalog::hang::Container as MuxContainer;
		use moq_mux::container::{Producer, ts};
		use moq_net::Timestamp;

		// Short enough to keep the test quick, long enough that the release instants
		// either side of the rewind are separated by more than clock noise.
		const LATENCY: Duration = Duration::from_millis(300);
		// Ten minutes in, the span from the controlled-rewind evidence on #2833.
		const OFFSET: u64 = 600_000_000;

		let origin = moq_net::Origin::random().produce();
		let mut broadcast = origin
			.create_broadcast("rewind", moq_net::broadcast::Route::new().with_announce(true))
			.unwrap();
		let mut catalog = moq_mux::catalog::Producer::with_catalog(
			&mut broadcast,
			moq_mux::catalog::hang::Catalog::<ts::Ext>::default(),
		)
		.unwrap();
		let track = broadcast
			.create_track(broadcast.unique_name(".aac"), hang::container::track_info())
			.unwrap();
		{
			let mut guard = catalog.lock();
			let mut config = hang::catalog::AudioConfig::new(hang::catalog::AAC { profile: 2 }, 48_000, 2);
			config.container = hang::catalog::Container::Legacy;
			guard.audio.renditions.insert(track.name().to_string(), config);
		}
		let mut producer = Producer::new(track, MuxContainer::Legacy);

		// 100ms audio frames in one-second groups.
		let mut write = |count: u64, offset: u64| {
			for i in 0..count {
				producer
					.write(Frame {
						timestamp: Timestamp::from_micros(offset + i * 100_000).unwrap(),
						duration: None,
						payload: Bytes::from_iter((0..180u16).map(|b| (b ^ i as u16) as u8)),
						keyframe: i % 10 == 0,
					})
					.unwrap();
				if i % 10 == 9 {
					producer.cut(None).unwrap();
				}
			}
		};

		let probe = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
		let addr: SocketAddr = probe.local_addr().unwrap();
		drop(probe);

		let mut server = Server::bind(addr, LATENCY).await.unwrap();
		let caller = tokio::spawn(async move {
			SrtSocket::builder()
				.call(addr, Some("#!::r=rewind,m=request"))
				.await
				.unwrap()
		});

		let Request::Subscribe(subscribe) = server.accept().await.expect("an SRT request") else {
			panic!("m=request must create a subscribe request");
		};
		let consumer = origin.consume();
		let egress = tokio::spawn(async move { subscribe.accept(&consumer, "rewind").await });
		let mut receiver = caller.await.unwrap();

		// Three seconds of program, then wait until the receiver is actually playing it
		// so the rewind lands a real wall-clock distance after the first packet.
		write(30, OFFSET);
		let mut received = Vec::new();
		let first = tokio::time::timeout(Duration::from_secs(10), receiver.next())
			.await
			.expect("the first payload never arrived")
			.expect("the SRT egress closed early")
			.unwrap();
		received.push(first);

		// The publisher restarts at the top of its source.
		write(30, 0);

		// Drain until the muxer flags the break, which it does exactly once, on the new
		// generation's leading clock packet.
		let boundary = loop {
			let payload = tokio::time::timeout(Duration::from_secs(10), receiver.next())
				.await
				.expect("the rewound generation never arrived")
				.expect("the SRT egress closed before the rewind")
				.unwrap();
			received.push(payload);
			if let Some(index) = received.iter().position(|(_, payload)| flags_a_break(payload)) {
				break index;
			}
		};

		assert!(boundary > 0, "the break cannot be in the connection's first payload");
		assert!(
			received[boundary].0 > received[0].0 + Duration::from_millis(100),
			"the rewound generation must be stamped at the wall clock it was muxed at, \
			 not back at the first packet ({:?} after it)",
			received[boundary].0.saturating_duration_since(received[0].0),
		);
		assert!(
			received[boundary].0 > received[boundary - 1].0,
			"the new generation starts its own payload rather than joining the old tail's",
		);

		drop(receiver);
		egress.abort();
	}

	/// Whether a payload carries a TS packet whose adaptation field sets
	/// `discontinuity_indicator`: the muxer flags the new generation's leading clock
	/// packet with it and nothing else.
	fn flags_a_break(payload: &[u8]) -> bool {
		payload
			.chunks_exact(188)
			.any(|packet| packet[3] & 0x20 != 0 && packet[4] > 0 && packet[5] & 0x80 != 0)
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
