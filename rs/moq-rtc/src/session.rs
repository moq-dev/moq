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
use std::time::Instant;

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
	/// The local address to report to str0m as each datagram's destination. MUST
	/// equal one of the local ICE candidates we advertised, not the socket's bind
	/// address: str0m drops a STUN binding request whose destination doesn't match
	/// a host candidate ("unknown interface"), and the shared mux socket binds a
	/// wildcard (`0.0.0.0`) while advertising a concrete IP.
	local: SocketAddr,
	/// Inbound datagrams routed to this session (demux on the server, a 1:1
	/// reader on the client). `None` from `recv` means every sender dropped, so
	/// the session is done.
	inbound: mpsc::Receiver<Packet>,
	role: MediaRole,
	/// Egress write requests. `Some` only for [`MediaRole::Egress`]
	/// sessions; pumps send frames here, the main loop forwards them into
	/// str0m's [`Writer`](str0m::media::Writer).
	writes_rx: Option<mpsc::Receiver<WriteRequest>>,
}

impl Session {
	/// Convenience for the ingest case (WHIP server, WHEP client). `local` is the
	/// advertised ICE candidate address (see the field docs), not the socket bind.
	pub fn ingest(
		rtc: Rtc,
		socket: Arc<UdpSocket>,
		local: SocketAddr,
		inbound: mpsc::Receiver<Packet>,
		sink: Box<dyn MediaSink>,
	) -> Self {
		Self {
			rtc,
			socket,
			local,
			inbound,
			role: MediaRole::Ingest(sink),
			writes_rx: None,
		}
	}

	/// Convenience for the egress case (WHEP server, WHIP client). `local` is the
	/// advertised ICE candidate address (see the field docs), not the socket bind.
	pub fn egress(
		rtc: Rtc,
		socket: Arc<UdpSocket>,
		local: SocketAddr,
		inbound: mpsc::Receiver<Packet>,
		mut source: EgressSource,
	) -> Self {
		let writes_rx = source.take_writes();
		Self {
			rtc,
			socket,
			local,
			inbound,
			role: MediaRole::Egress(Box::new(source)),
			writes_rx: Some(writes_rx),
		}
	}

	pub async fn run(mut self) -> Result<()> {
		// The local address str0m tags each inbound packet with -- the advertised
		// ICE candidate, NOT the socket bind (see the `local` field docs).
		let local = self.local;

		loop {
			let timeout = match self.rtc.poll_output().map_err(Error::Rtc)? {
				Output::Timeout(t) => t,
				Output::Transmit(t) => {
					if let Err(err) = self.socket.send_to(&t.contents, t.destination).await {
						tracing::warn!(%err, dst = %t.destination, "send failed");
					}
					continue;
				}
				Output::Event(event) => {
					self.handle_event(event)?;
					continue;
				}
			};

			let now = Instant::now();
			let duration = timeout.saturating_duration_since(now);
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
				if let MediaRole::Ingest(sink) = &mut self.role {
					let timestamp_us = media_time_to_micros(&data.time);
					sink.on_frame(
						data.mid,
						codec::Frame {
							timestamp_us,
							payload: data.data.into(),
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
	let mut config = str0m::RtcConfig::new().clear_codecs();
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
