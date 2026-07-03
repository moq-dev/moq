//! str0m session driver shared by every HTTP role / media direction.
//!
//! str0m is sans-IO, so we drive the [`str0m::Rtc`] instance from a tokio
//! task that owns a UDP socket. [`Session::run`] alternates between
//! [`Rtc::poll_output`] (drain pending transmits / events) and
//! [`Rtc::handle_input`] (feed UDP packets or timeouts).
//!
//! The session itself doesn't care whether the [`Rtc`] was populated by
//! accepting an SDP offer (server side) or by minting one and posting it
//! to a remote URL (client side), or whether the media flow is RTP-in
//! ([`MediaSink`]) or RTP-out ([`crate::egress::EgressSource`]).

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::{Event, IceConnectionState, Input, Output, Rtc, net::Receive};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::egress::{EgressSource, WriteRequest};
use crate::{Error, Result, codec};

/// One inbound UDP datagram plus its source address, the unit fed to a session.
/// The [`server`](crate::server) paths get these from the shared-socket demux
/// (`crate::server::mux`); the client paths get them from a 1:1 reader
/// ([`spawn_socket_reader`]).
pub(crate) type Packet = (Vec<u8>, SocketAddr);

/// Bound on a session's inbound datagram queue, sized like a socket buffer:
/// past this, datagrams are dropped rather than buffered (WebRTC tolerates loss
/// and a stalled session must not grow memory without limit).
pub(crate) const SESSION_INBOX: usize = 256;

/// str0m's outbound video buffer depth (packets), which also backs NACK resends.
/// Raised above the str0m default (1000) so a late-joining peer can recover a
/// large keyframe and the rest of the current group via NACK; see
/// [`rtc_config_with_codecs`].
const EGRESS_SEND_BUFFER_VIDEO: usize = 3000;

/// Backstop deadline for a session to reach a connected ICE state, covering the one
/// case str0m's ICE agent deliberately never times out: a peer that answers the SDP
/// but provides NO remote candidates and sends nothing (an abandoned WHIP/WHEP
/// offer, or a probe that only exercises signalling). str0m DOES end a connection
/// whose candidate pairs were tried and exhausted -- the agent goes to
/// `IceConnectionState::Disconnected` (handled in `handle_event`) after
/// ~`StunTiming::timeout()` (~21s at the defaults). But when `remote_candidates`
/// stays empty the agent treats the session as "still possible" forever (trickle
/// ICE: more candidates could arrive), so it sits in `Checking` indefinitely,
/// pinning this task, its broadcast announcement, and its mux registration. Nothing
/// upstream ends it, so we do. Set ABOVE str0m's ~21s pair-exhaustion so a
/// connection that actually started checks is ended by str0m's native path (and a
/// slow-but-real TURN/lossy peer isn't clipped); this only fires for the
/// never-any-candidate case.
const ICE_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(30);

/// Receives `MediaData` events from str0m and dispatches to the right codec
/// [`Bridge`](codec::Bridge). Used as the per-session sink in [`Session::run`]
/// for any flow where RTP arrives from the peer (`server publish` / WHIP
/// server, `client subscribe` / WHEP client).
pub trait MediaSink: Send {
	/// Called once str0m has confirmed which codec is on which `mid`.
	fn on_track(
		&mut self,
		mid: str0m::media::Mid,
		kind: str0m::media::MediaKind,
		codec: str0m::format::Codec,
		audio_params: Option<(u32, u32)>,
	) -> Result<()>;

	/// Called on each [`MediaData`](str0m::media::MediaData) event. The session
	/// loop has already converted the timestamp to microseconds.
	fn on_frame(&mut self, mid: str0m::media::Mid, frame: codec::Frame) -> Result<()>;
}

/// What the session does with the negotiated media stream.
#[non_exhaustive]
pub enum MediaRole {
	/// RTP-in: dispatch peer frames into a [`MediaSink`].
	Ingest(Box<dyn MediaSink>),
	/// RTP-out: pull frames from a [`crate::egress::EgressSource`] and forward to the peer.
	Egress(Box<EgressSource>),
}

/// Drives a [`Rtc`] instance until it ends.
///
/// The caller pre-populates the `Rtc` with whatever SDP exchange they need.
/// Sends go out the (possibly shared) `socket`; inbound datagrams arrive on
/// `inbound` rather than being read off the socket directly, so several
/// sessions can share one socket behind the `crate::server::mux`.
pub struct Session {
	rtc: Rtc,
	/// Send side. Shared across sessions on the server (the mux socket); owned
	/// 1:1 on the client. Receiving happens via `inbound`, not this socket.
	socket: Arc<UdpSocket>,
	/// The local ICE candidates we advertised. Each inbound datagram is tagged
	/// (for str0m) with the candidate whose address family matches the packet's
	/// source, so a dual-stack peer reaching us over IPv6 isn't told the packet
	/// arrived on an IPv4 host candidate. MUST be the advertised candidates, not
	/// the socket's bind address: str0m drops a STUN binding request whose
	/// destination doesn't match a host candidate ("unknown interface"), and the
	/// shared mux socket binds a wildcard (`0.0.0.0`) while advertising concrete
	/// IPs. Never empty (falls back to the bound address).
	locals: Vec<SocketAddr>,
	/// Inbound datagrams routed to this session (demux on the server, a 1:1
	/// reader on the client). `None` from `recv` means every sender dropped, so
	/// the session is done.
	inbound: mpsc::Receiver<Packet>,
	role: MediaRole,
	/// Egress write requests. `Some` only for [`MediaRole::Egress`]
	/// sessions; pumps send frames here, the main loop forwards them into
	/// str0m's [`Writer`](str0m::media::Writer).
	writes_rx: Option<mpsc::Receiver<WriteRequest>>,
	/// Rebases each ingested track's raw RTP timestamps onto one session
	/// timeline so audio and video stay in sync. Unused by egress sessions.
	clock: IngestClock,
}

impl Session {
	/// Convenience for the ingest case (WHIP server, WHEP client). `locals` are the
	/// advertised ICE candidates (see the field docs), not the socket bind.
	pub fn ingest(
		rtc: Rtc,
		socket: Arc<UdpSocket>,
		locals: Vec<SocketAddr>,
		inbound: mpsc::Receiver<Packet>,
		sink: Box<dyn MediaSink>,
	) -> Self {
		Self {
			rtc,
			socket,
			locals,
			inbound,
			role: MediaRole::Ingest(sink),
			writes_rx: None,
			clock: IngestClock::default(),
		}
	}

	/// Convenience for the egress case (WHEP server, WHIP client). `locals` are the
	/// advertised ICE candidates (see the field docs), not the socket bind.
	pub fn egress(
		rtc: Rtc,
		socket: Arc<UdpSocket>,
		locals: Vec<SocketAddr>,
		inbound: mpsc::Receiver<Packet>,
		mut source: EgressSource,
	) -> Self {
		let writes_rx = source.take_writes();
		Self {
			rtc,
			socket,
			locals,
			inbound,
			role: MediaRole::Egress(Box::new(source)),
			writes_rx: Some(writes_rx),
			clock: IngestClock::default(),
		}
	}

	pub async fn run(mut self) -> Result<()> {
		let started = Instant::now();
		let mut connected = false;
		loop {
			// A dead Rtc (DTLS/SDP failure, explicit disconnect) makes poll_output
			// return a never-firing timeout instead of erroring, which would hang
			// this task forever holding the broadcast announcement + mux
			// registration. Bail so those release.
			if !self.rtc.is_alive() {
				return Err(Error::SessionClosed);
			}

			// Abort a session that never finishes connecting (see
			// ICE_ESTABLISH_TIMEOUT); once connected, str0m's own timeouts take over.
			if !connected && started.elapsed() >= ICE_ESTABLISH_TIMEOUT {
				return Err(Error::IceTimeout);
			}

			let timeout = match self.rtc.poll_output().map_err(Error::Rtc)? {
				Output::Timeout(t) => t,
				Output::Transmit(t) => {
					if let Err(err) = self.socket.send_to(&t.contents, t.destination).await {
						tracing::warn!(%err, dst = %t.destination, "send failed");
					}
					continue;
				}
				Output::Event(event) => {
					if let Event::IceConnectionStateChange(state) = &event {
						connected |= state.is_connected();
					}
					self.handle_event(event)?;
					continue;
				}
			};

			let now = Instant::now();
			let mut duration = timeout.saturating_duration_since(now);
			// While still connecting, never sleep past the establishment deadline, so
			// the check above fires on time even if str0m scheduled a far-off timeout.
			if !connected {
				duration = duration.min(ICE_ESTABLISH_TIMEOUT.saturating_sub(started.elapsed()));
			}
			if duration.is_zero() {
				self.rtc.handle_input(Input::Timeout(now)).map_err(Error::Rtc)?;
				continue;
			}

			// Wait for the earliest of: an inbound UDP packet, an egress
			// write request (if egress), or the str0m-requested timeout.
			tokio::select! {
				biased;

				// Egress writes get drained promptly. Without `biased` an
				// idle socket select could starve them.
				Some(req) = async {
					match self.writes_rx.as_mut() {
						Some(rx) => rx.recv().await,
						None => std::future::pending::<Option<WriteRequest>>().await,
					}
				} => {
					crate::egress::dispatch(&mut self.rtc, req, Instant::now());
				}

				packet = self.inbound.recv() => {
					match packet {
						Some((data, src)) => {
							let now = Instant::now();
							// Tag the packet with the advertised candidate matching its
							// address family, not the socket bind (see the `locals` docs).
							let local = pick_local(&self.locals, src);
							let recv = Receive::new(str0m::net::Protocol::Udp, src, local, &data)
								.map_err(Error::RtcInput)?;
							self.rtc.handle_input(Input::Receive(now, recv)).map_err(Error::Rtc)?;
						}
						// Every sender dropped: the demux unregistered us (or the
						// 1:1 reader stopped). Nothing more will arrive, so end.
						None => return Err(Error::SessionClosed),
					}
				}

				_ = tokio::time::sleep(duration) => {
					self.rtc
						.handle_input(Input::Timeout(Instant::now()))
						.map_err(Error::Rtc)?;
				}
			}
		}
	}

	fn handle_event(&mut self, event: Event) -> Result<()> {
		match event {
			Event::IceConnectionStateChange(state) => {
				tracing::debug!(?state, "ice state");
				if state == IceConnectionState::Disconnected {
					return Err(Error::SessionClosed);
				}
			}
			Event::MediaAdded(added) => self.handle_media_added(added)?,
			Event::MediaData(data) => {
				// `clock` and `role` are disjoint fields, so the borrow checker lets
				// us rebase the (random, per-track) RTP base and feed the sink in one
				// block; egress sessions never get here so the clock stays untouched.
				if let MediaRole::Ingest(sink) = &mut self.role {
					let media_us = media_time_to_micros(&data.time);
					let timestamp_us = self.clock.normalize(data.mid, data.network_time, media_us);
					sink.on_frame(
						data.mid,
						codec::Frame {
							timestamp_us,
							payload: bytes::Bytes::from_owner(data.data),
						},
					)?;
				}
			}
			Event::KeyframeRequest(req) => {
				// PLI / FIR from the egress peer. For v1 we just log and
				// rely on the next natural keyframe from the MoQ source.
				tracing::debug!(?req, "keyframe request from peer");
			}
			_ => {}
		}
		Ok(())
	}

	fn handle_media_added(&mut self, added: str0m::media::MediaAdded) -> Result<()> {
		// str0m's CodecConfig is the negotiated set; pick the first
		// codec advertised for this `mid`.
		let pt = self.rtc.media(added.mid).and_then(|m| m.remote_pts().first().copied());
		let params = pt.and_then(|pt| self.rtc.codec_config().params().iter().find(|p| p.pt() == pt).copied());
		let params = match params {
			Some(p) => p,
			None => {
				tracing::warn!(?added.mid, "no codec params for media; ignoring");
				return Ok(());
			}
		};
		let spec = params.spec();
		let codec = spec.codec;

		match &mut self.role {
			MediaRole::Ingest(sink) => {
				let audio_params = if codec.is_audio() {
					Some((spec.clock_rate.get(), spec.channels.unwrap_or(1) as u32))
				} else {
					None
				};
				sink.on_track(added.mid, added.kind, codec, audio_params)?;
			}
			MediaRole::Egress(source) => {
				source.on_track(added.mid, codec, params.pt(), spec.clock_rate)?;
			}
		}
		Ok(())
	}
}

/// Per-session clock that rebases each ingested track's raw RTP timestamps onto
/// one timeline so audio and video stay in sync.
///
/// str0m hands us the RTP header timestamp verbatim
/// ([`MediaData::time`](str0m::media::MediaData::time)). Per RFC 3550 that base
/// is random and independent for each track, and str0m applies no RTCP
/// sender-report correlation, so publishing the values as-is would desync audio
/// from video (their bases differ by hours) and start the broadcast at an
/// arbitrary offset. We anchor each track on its first frame to that packet's
/// arrival time (relative to the first frame seen in the whole session), then
/// advance within the track by the RTP delta (str0m extends the 32-bit RTP
/// timestamp with a roll-over counter, so the delta is wrap-safe). The first
/// frame of the session maps to 0.
#[derive(Default)]
pub(crate) struct IngestClock {
	/// Arrival time of the first frame seen in the session; the timeline origin.
	epoch: Option<Instant>,
	/// Per-track additive offset (microseconds) applied to the raw RTP time.
	offsets: HashMap<str0m::media::Mid, i64>,
}

impl IngestClock {
	/// Map a raw RTP-derived microsecond timestamp onto the session timeline.
	/// `arrival` is the packet's network time
	/// ([`MediaData::network_time`](str0m::media::MediaData::network_time)).
	fn normalize(&mut self, mid: str0m::media::Mid, arrival: Instant, media_us: u64) -> u64 {
		let epoch = *self.epoch.get_or_insert(arrival);
		let offset = *self.offsets.entry(mid).or_insert_with(|| {
			// Signed wall delta from the epoch: a track whose first frame we dequeue
			// after the epoch frame may have actually arrived *before* it, and that
			// lead must pull its timeline earlier (not clamp to the epoch via an
			// unsigned subtraction) so it stays in sync.
			let wall_us = if arrival >= epoch {
				arrival.duration_since(epoch).as_micros() as i64
			} else {
				-(epoch.duration_since(arrival).as_micros() as i64)
			};
			wall_us - media_us as i64
		});
		(media_us as i64 + offset).max(0) as u64
	}
}

/// Log a finished session at the right level: an ordinary peer disconnect
/// ([`Error::SessionClosed`]) is debug, a genuine failure is a warning. Keeps
/// normal WebRTC churn out of the warning stream. `role` labels the path
/// (e.g. `"whip server"`).
pub(crate) fn log_session_end(role: &str, result: &Result<()>) {
	match result {
		Ok(()) | Err(Error::SessionClosed) => tracing::debug!(role, "session ended"),
		// An abandoned offer (peer answered but never connected) is normal churn, not
		// a failure: keep it out of the warning stream.
		Err(Error::IceTimeout) => tracing::debug!(role, "session ended: ICE never connected"),
		Err(err) => tracing::warn!(%err, role, "session ended"),
	}
}

/// Pick the advertised local candidate to tag an inbound packet with: the first
/// one whose address family matches `src`, falling back to the first candidate
/// (the list is never empty). Keeps a dual-stack peer's packets tagged with a
/// same-family host candidate so str0m's ICE pairing stays consistent.
fn pick_local(locals: &[SocketAddr], src: SocketAddr) -> SocketAddr {
	locals
		.iter()
		.find(|l| l.is_ipv4() == src.is_ipv4())
		.copied()
		.unwrap_or(locals[0])
}

/// Convert a str0m [`MediaTime`](str0m::media::MediaTime) to microseconds.
fn media_time_to_micros(time: &str0m::media::MediaTime) -> u64 {
	// MediaTime stores `numer / denom` seconds; cast through i128 so the
	// product doesn't overflow at 90 kHz video timestamps.
	let numer = time.numer() as i128;
	let denom = time.denom() as i128;
	if denom == 0 {
		return 0;
	}
	let micros = (numer.saturating_mul(1_000_000)) / denom;
	micros.max(0) as u64
}

/// Type-erased map of `Mid` -> codec bridge, populated as `MediaAdded`
/// events arrive on the ingest side.
pub(crate) struct Bridges {
	inner: HashMap<str0m::media::Mid, Box<dyn codec::Bridge>>,
}

impl Bridges {
	pub fn new() -> Self {
		Self { inner: HashMap::new() }
	}

	pub fn insert(&mut self, mid: str0m::media::Mid, bridge: Box<dyn codec::Bridge>) {
		self.inner.insert(mid, bridge);
	}

	pub fn push(&mut self, mid: str0m::media::Mid, frame: codec::Frame) -> Result<()> {
		if let Some(bridge) = self.inner.get_mut(&mid) {
			bridge.push(frame)?;
		}
		Ok(())
	}
}

/// Build a [`Rtc`] with `CodecConfig` restricted to the supplied codecs.
///
/// Used by the two egress paths so we don't advertise codecs we have no
/// source for in the catalog (WHIP client) or accept incoming codecs we
/// can't fulfil (WHEP server). For both, the negotiated SDP intersects with
/// what we can actually deliver, so `MediaAdded` only fires for codecs that
/// [`crate::egress::EgressSource`] can match to a rendition.
pub fn rtc_config_with_codecs(codecs: &[str0m::format::Codec]) -> str0m::RtcConfig {
	use str0m::format::Codec;
	// str0m fulfils NACK resends from the video send buffer (default 1000
	// packets). MoQ has no PLI path back to the publisher, so a late joiner's
	// recovery is whatever the peer can NACK out of this buffer while the current
	// group is still in flight. Widen it so a large keyframe plus the rest of the
	// group stays recoverable instead of aging out after ~1000 packets.
	let mut config = str0m::RtcConfig::new()
		.clear_codecs()
		.set_send_buffer_video(EGRESS_SEND_BUFFER_VIDEO);
	for c in codecs {
		config = match c {
			Codec::Opus => config.enable_opus(true),
			Codec::H264 => config.enable_h264(true),
			Codec::H265 => config.enable_h265(true),
			Codec::Vp8 => config.enable_vp8(true),
			Codec::Vp9 => config.enable_vp9(true),
			Codec::Av1 => config.enable_av1(true),
			// Any other codec str0m grows is one we have no egress source for.
			_ => config,
		};
	}
	config
}

/// Build a codec-restricted [`Rtc`] for the client egress path (which lets
/// str0m mint its own ICE credentials). The server egress path uses
/// [`rtc_config_with_codecs`] directly so it can inject the mux's known
/// credentials before building.
pub fn rtc_with_codecs(codecs: &[str0m::format::Codec]) -> Rtc {
	rtc_config_with_codecs(codecs).build(std::time::Instant::now())
}

/// Bind an ephemeral UDP socket for a single client session and return it
/// (shared with its [reader task](spawn_socket_reader)) plus the ICE candidates
/// to advertise.
///
/// The client paths are 1:1 (one socket per dialed session, no demux); the
/// server paths share one socket via `crate::server::mux` instead. `advertise`
/// IPs are used verbatim (reusing the bound port); empty falls back to whatever
/// address the OS picked (loopback only).
pub async fn bind_udp(advertise: &[SocketAddr]) -> Result<(Arc<UdpSocket>, Vec<SocketAddr>)> {
	let socket = UdpSocket::bind(("0.0.0.0", 0)).await?;
	let local = socket.local_addr()?;
	let candidates = if advertise.is_empty() {
		vec![local]
	} else {
		// Reuse the bound port across each advertised IP, since str0m's ICE
		// agent picks the destination port from the candidate it's pairing
		// against.
		advertise
			.iter()
			.map(|addr| SocketAddr::new(addr.ip(), local.port()))
			.collect()
	};
	Ok((Arc::new(socket), candidates))
}

/// Spawn a 1:1 reader pumping every datagram from `socket` into a channel, for
/// the client paths (one socket per session, so no demux is needed). Mirrors the
/// inbound side of `crate::server::mux` for a single session.
pub fn spawn_socket_reader(socket: Arc<UdpSocket>) -> mpsc::Receiver<Packet> {
	let (tx, rx) = mpsc::channel(SESSION_INBOX);
	tokio::spawn(async move {
		let mut buf = vec![0u8; 65_535];
		loop {
			match socket.recv_from(&mut buf).await {
				// Bounded like a socket buffer: drop on full, stop once the
				// session's receiver is gone.
				Ok((len, src)) => {
					if let Err(mpsc::error::TrySendError::Closed(_)) = tx.try_send((buf[..len].to_vec(), src)) {
						break;
					}
				}
				Err(err) => {
					tracing::warn!(%err, "webrtc client socket recv failed");
					break;
				}
			}
		}
	});
	rx
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use str0m::media::Mid;

	use super::*;

	#[test]
	fn pick_local_matches_address_family() {
		let v4: SocketAddr = "1.2.3.4:5000".parse().unwrap();
		let v6: SocketAddr = "[2001:db8::1]:5000".parse().unwrap();
		let locals = vec![v4, v6];
		let src_v4: SocketAddr = "9.9.9.9:1".parse().unwrap();
		let src_v6: SocketAddr = "[2001:db8::2]:1".parse().unwrap();
		assert_eq!(pick_local(&locals, src_v4), v4);
		assert_eq!(pick_local(&locals, src_v6), v6);
		// No same-family candidate falls back to the first.
		assert_eq!(pick_local(&[v4], src_v6), v4);
	}

	#[test]
	fn ingest_clock_rebases_first_frame_to_zero() {
		let mut clock = IngestClock::default();
		let mid = Mid::from("0");
		let t0 = Instant::now();
		// Raw RTP base is a large random value; the first frame must map to 0.
		assert_eq!(clock.normalize(mid, t0, 5_000_000_000), 0);
	}

	#[test]
	fn ingest_clock_tracks_rtp_delta_within_track() {
		let mut clock = IngestClock::default();
		let mid = Mid::from("0");
		let t0 = Instant::now();
		assert_eq!(clock.normalize(mid, t0, 5_000_000_000), 0);
		// A later frame advances by the RTP delta, not by arrival jitter.
		let arrival = t0 + Duration::from_millis(17); // jittered arrival, ignored after anchor
		assert_eq!(clock.normalize(mid, arrival, 5_000_020_000), 20_000);
	}

	#[test]
	fn ingest_clock_keeps_tracks_in_sync_via_arrival() {
		let mut clock = IngestClock::default();
		let audio = Mid::from("0");
		let video = Mid::from("1");
		let t0 = Instant::now();
		// Audio anchors the session at 0 with its own random RTP base.
		assert_eq!(clock.normalize(audio, t0, 1_000_000_000), 0);
		// Video's first frame arrives 5 ms later with an unrelated RTP base; it
		// must land at +5 ms on the shared timeline, not at video's raw base.
		let video_arrival = t0 + Duration::from_millis(5);
		assert_eq!(clock.normalize(video, video_arrival, 8_000_000_000), 5_000);
		// And then track its own RTP delta.
		assert_eq!(
			clock.normalize(video, video_arrival + Duration::from_millis(33), 8_000_033_000),
			38_000
		);
	}

	#[test]
	fn ingest_clock_handles_track_arriving_before_epoch() {
		let mut clock = IngestClock::default();
		let audio = Mid::from("0");
		let video = Mid::from("1");
		let t0 = Instant::now();
		// Audio's MediaData is dequeued first and sets the epoch at t0.
		assert_eq!(clock.normalize(audio, t0, 1_000_000), 0);
		// Video's first frame actually arrived 5 ms *before* the epoch. Its lead
		// pulls the start below zero (clamped to 0), and a frame 33 ms into video
		// lands 28 ms onto the shared timeline (33 ms - the 5 ms head start).
		let video_arrival = t0 - Duration::from_millis(5);
		assert_eq!(clock.normalize(video, video_arrival, 8_000_000), 0);
		assert_eq!(
			clock.normalize(video, video_arrival + Duration::from_millis(33), 8_033_000),
			28_000
		);
	}
}
