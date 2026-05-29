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
use std::time::Instant;

use str0m::{Event, IceConnectionState, Input, Output, Rtc, net::Receive};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::egress::{EgressSource, WriteRequest};
use crate::{Error, Result, codec};

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

/// Drives a [`Rtc`] instance on a UDP socket until it ends.
///
/// The caller pre-populates the `Rtc` with whatever SDP exchange they need;
/// this just owns the socket and the media role.
pub struct Session {
	rtc: Rtc,
	socket: UdpSocket,
	role: MediaRole,
	/// Egress write requests. `Some` only for [`MediaRole::Egress`]
	/// sessions; pumps send frames here, the main loop forwards them into
	/// str0m's [`Writer`](str0m::media::Writer).
	writes_rx: Option<mpsc::Receiver<WriteRequest>>,
}

impl Session {
	/// Convenience for the ingest case (WHIP server, WHEP client).
	pub fn ingest(rtc: Rtc, socket: UdpSocket, sink: Box<dyn MediaSink>) -> Self {
		Self {
			rtc,
			socket,
			role: MediaRole::Ingest(sink),
			writes_rx: None,
		}
	}

	/// Convenience for the egress case (WHEP server, WHIP client).
	pub fn egress(rtc: Rtc, socket: UdpSocket, mut source: EgressSource) -> Self {
		let writes_rx = source.take_writes();
		Self {
			rtc,
			socket,
			role: MediaRole::Egress(Box::new(source)),
			writes_rx: Some(writes_rx),
		}
	}

	pub async fn run(mut self) -> Result<()> {
		// Buffer for one UDP datagram (max v4/v6 payload size).
		let mut buf = vec![0u8; 65_535];

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

				read = self.socket.recv_from(&mut buf) => {
					match read {
						Ok((len, src)) => {
							let local = self.socket.local_addr()?;
							let now = Instant::now();
							let recv = Receive::new(str0m::net::Protocol::Udp, src, local, &buf[..len])
								.map_err(Error::RtcInput)?;
							self.rtc.handle_input(Input::Receive(now, recv)).map_err(Error::Rtc)?;
						}
						Err(err) => return Err(err.into()),
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

/// Bind a UDP socket on `0.0.0.0:0` and return both the socket and the
/// listed ICE candidates the caller should advertise.
///
/// If `advertise` is non-empty, those addresses are used verbatim as
/// ICE host candidates (typically the gateway's public IPs). Otherwise we
/// fall back to whatever the OS picked.
pub async fn bind_udp(advertise: &[SocketAddr]) -> Result<(UdpSocket, Vec<SocketAddr>)> {
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
	Ok((socket, candidates))
}
