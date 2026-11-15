use crate::vendor::str0m;
use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;

use kio::clock::Instant;
use str0m::net::{Protocol, Receive};
use str0m::{Event, IceConnectionState, Input, Output, Rtc};
use tokio::net::UdpSocket;

use super::{Inbound, Socket};

/// Backstop deadline for reaching a connected ICE state, covering the one case
/// str0m's agent deliberately never times out: a peer that provides no remote
/// candidates and sends nothing. str0m ends a connection whose candidate pairs were
/// tried and exhausted (after ~21s at its defaults), but with no remote candidates
/// it treats the session as still possible forever, since trickle ICE could add
/// one. Set above str0m's own exhaustion so a slow but real peer is not clipped.
const ESTABLISH_TIMEOUT: Duration = Duration::from_secs(30);

/// Why an [`Agent`] stopped.
#[derive(Clone, Debug, thiserror::Error)]
pub enum Error {
	/// The peer disconnected, str0m closed the connection, or the inbound queue ended.
	#[error("peer connection closed")]
	Closed,

	/// ICE never connected before the establishment deadline, or before the DTLS
	/// handshake gave up waiting for it.
	#[error("ICE did not connect in time")]
	Timeout,

	/// str0m failed (SDP, DTLS, or ICE state).
	#[error("rtc error: {0}")]
	Rtc(String),
}

impl Error {
	fn rtc(err: impl std::fmt::Display) -> Self {
		Self::Rtc(err.to_string())
	}
}

/// Where the connection is in its life.
enum Phase {
	/// ICE has yet to connect; the [`ESTABLISH_TIMEOUT`] runs from `started`.
	Establishing {
		started: Instant,
	},
	Connected,
	/// The connection ended, for this reason: every later [`Agent::next`] reports it.
	Failed(Error),
}

/// Drives one [`Rtc`] over a UDP socket until the connection ends.
pub struct Agent {
	rtc: Rtc,
	/// Send side, possibly shared by several agents (a demuxed server socket).
	socket: Arc<UdpSocket>,
	/// Whether `socket` is AF_INET6, which needs IPv4 destinations mapped.
	socket_v6: bool,
	/// The host candidates advertised to the peer. Each inbound datagram is tagged
	/// with the one whose family matches its source: str0m drops a STUN request
	/// whose destination is not a local candidate, and a wildcard bind is not one.
	/// Never empty.
	locals: Vec<SocketAddr>,
	inbound: Inbound,
	events: VecDeque<Event>,
	/// When str0m next wants a timeout, as of the last drain.
	timeout: Instant,
	/// Datagrams the socket refused for lack of buffer space, sent first next time.
	outbox: VecDeque<(Vec<u8>, SocketAddr)>,
	phase: Phase,
}

impl Agent {
	/// Drive `rtc` over `socket`.
	pub fn new(rtc: Rtc, socket: Socket) -> std::io::Result<Self> {
		let Socket {
			udp: socket,
			locals,
			inbound,
		} = socket;
		let socket_v6 = socket.local_addr()?.is_ipv6();
		let now = Instant::now();
		let mut agent = Self {
			rtc,
			socket,
			socket_v6,
			locals,
			inbound,
			events: VecDeque::new(),
			timeout: now,
			outbox: VecDeque::new(),
			phase: Phase::Establishing { started: now },
		};
		// Signaling already changed the connection.
		agent.drain();
		Ok(agent)
	}

	/// The connection being driven, read only.
	pub fn rtc(&self) -> &Rtc {
		&self.rtc
	}

	/// Change the connection through a guard that drains what the change produced
	/// when dropped: transmits are sent, and events wait for [`Agent::next`].
	pub fn rtc_mut(&mut self) -> RtcMut<'_> {
		RtcMut(self)
	}

	/// Drive the connection until str0m surfaces an [`Event`]. ICE disconnection is
	/// reported as [`Error::Closed`] instead.
	///
	/// Cancel safe: the connection is always drained between calls, and a datagram
	/// the socket could not take yet is kept and sent first on the next call.
	pub async fn next(&mut self) -> Result<Event, Error> {
		loop {
			if let Some(event) = self.events.pop_front() {
				return Ok(event);
			}
			if let Phase::Failed(err) = &self.phase {
				return Err(err.clone());
			}
			while let Some((data, dst)) = self.outbox.front() {
				match self.socket.send_to(data, *dst).await {
					Ok(_) => {}
					Err(err) => tracing::warn!(%err, dst = %dst, "send failed"),
				}
				self.outbox.pop_front();
			}

			// A dead Rtc (DTLS failure, explicit disconnect) reports a never-firing
			// timeout instead of an error, which would park this loop forever.
			if !self.rtc.is_alive() {
				self.fail(Error::Closed);
				continue;
			}
			let establish = self.establish();
			if establish.is_some_and(|at| at <= Instant::now()) {
				self.fail(Error::Timeout);
				continue;
			}
			let deadline = establish.map_or(self.timeout, |at| at.min(self.timeout));

			let now = Instant::now();
			if deadline > now {
				tokio::select! {
					packet = self.inbound.packets.recv() => {
						let Some((data, src)) = packet else {
							// Every sender dropped: nothing more will arrive.
							self.fail(Error::Closed);
							continue;
						};
						self.receive(&data, src);
						continue;
					}
					_ = kio::clock::sleep_until(deadline) => {}
				}
			}

			let result = self.rtc_mut().handle_input(Input::Timeout(Instant::now().into_std()));
			if let Err(err) = result {
				self.fail(Error::rtc(err));
			}
		}
	}

	/// When [`ESTABLISH_TIMEOUT`] expires, or `None` once ICE connected or the connection ended.
	fn establish(&self) -> Option<Instant> {
		match &self.phase {
			Phase::Establishing { started } => Some(started.saturating_add(ESTABLISH_TIMEOUT)),
			_ => None,
		}
	}

	/// Poll str0m until it asks for a timeout: transmits are sent, events queued.
	fn drain(&mut self) {
		while !matches!(self.phase, Phase::Failed(_)) {
			let output = match self.rtc.poll_output() {
				Ok(output) => output,
				// Before ICE connects no DTLS record can reach the peer, so a DTLS failure
				// is its handshake giving up on retransmitting into the void. Its flights
				// (~31s, randomly jittered) race the establishment deadline, so report
				// either as ICE never connecting.
				Err(str0m::RtcError::Dtls(_)) if !matches!(self.phase, Phase::Connected) => {
					return self.fail(Error::Timeout);
				}
				Err(err) => return self.fail(Error::rtc(err)),
			};
			match output {
				// str0m runs on std's clock.
				Output::Timeout(timeout) => {
					self.timeout = Instant::from_std(timeout);
					return;
				}
				Output::Transmit(transmit) => self.send(&transmit.contents, transmit.destination),
				Output::Event(event) => {
					if let str0m::Event::IceConnectionStateChange(state) = &event {
						tracing::debug!(?state, "ice state");
						if *state == IceConnectionState::Disconnected {
							return self.fail(Error::Closed);
						}
						if state.is_connected() {
							self.phase = Phase::Connected;
						}
					}
					self.events.push_back(event);
				}
			}
		}
	}

	/// Keep the first reason the connection ended.
	fn fail(&mut self, err: Error) {
		if !matches!(self.phase, Phase::Failed(_)) {
			self.phase = Phase::Failed(err);
		}
	}

	/// Send without waiting, keeping the datagram for later when the socket is full.
	fn send(&mut self, data: &[u8], dst: SocketAddr) {
		let dst = to_family(dst, self.socket_v6);
		match self.socket.try_send_to(data, dst) {
			Ok(_) => {}
			// Changes can keep coming while the socket stays full, so the backlog is
			// bounded like the inbound queue; ICE, DTLS, and RTP recover the loss.
			Err(err) if err.kind() == std::io::ErrorKind::WouldBlock && self.outbox.len() < crate::ice::INBOX => {
				self.outbox.push_back((data.to_vec(), dst))
			}
			Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
				tracing::debug!(%dst, len = data.len(), "dropped a datagram: the socket is full")
			}
			Err(err) => tracing::warn!(%err, %dst, "send failed"),
		}
	}

	fn receive(&mut self, data: &[u8], src: SocketAddr) {
		let local = pick_local(&self.locals, src);
		// Anything can arrive on a public socket, so a datagram that is not STUN,
		// DTLS, or RTP is noise, not a failure.
		let Ok(receive) = Receive::new(Protocol::Udp, src, local, data) else {
			tracing::debug!(%src, len = data.len(), "dropped unrecognized datagram");
			return;
		};
		let result = self
			.rtc_mut()
			.handle_input(Input::Receive(Instant::now().into_std(), receive));
		if let Err(err) = result {
			self.fail(Error::rtc(err));
		}
	}
}

/// Mutable access to an [`Agent`]'s connection, which drains it when dropped.
pub struct RtcMut<'a>(&'a mut Agent);

impl Deref for RtcMut<'_> {
	type Target = Rtc;

	fn deref(&self) -> &Rtc {
		&self.0.rtc
	}
}

impl DerefMut for RtcMut<'_> {
	fn deref_mut(&mut self) -> &mut Rtc {
		&mut self.0.rtc
	}
}

impl Drop for RtcMut<'_> {
	fn drop(&mut self) {
		self.0.drain();
	}
}

/// Unmap an IPv4-mapped address to the `V4` it really is; others pass through.
pub fn canonical(addr: SocketAddr) -> SocketAddr {
	SocketAddr::new(addr.ip().to_canonical(), addr.port())
}

/// Adapt a destination to what the socket accepts: an AF_INET6 socket only sends
/// to IPv6, so an IPv4 destination goes back to its mapped form.
fn to_family(addr: SocketAddr, socket_v6: bool) -> SocketAddr {
	match addr.ip() {
		IpAddr::V4(ip) if socket_v6 => SocketAddr::new(IpAddr::V6(ip.to_ipv6_mapped()), addr.port()),
		_ => addr,
	}
}

/// The advertised candidate to tag an inbound datagram with: the first of the
/// source's family, falling back to the first, so a dual-stack peer's packets keep
/// a same-family host candidate and str0m's pairing stays consistent.
fn pick_local(locals: &[SocketAddr], src: SocketAddr) -> SocketAddr {
	locals
		.iter()
		.find(|l| l.is_ipv4() == src.is_ipv4())
		.copied()
		.unwrap_or(locals[0])
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn pick_local_matches_address_family() {
		let v4: SocketAddr = "1.2.3.4:5000".parse().unwrap();
		let v6: SocketAddr = "[2001:db8::1]:5000".parse().unwrap();
		let locals = vec![v4, v6];
		assert_eq!(pick_local(&locals, "9.9.9.9:1".parse().unwrap()), v4);
		assert_eq!(pick_local(&locals, "[2001:db8::2]:1".parse().unwrap()), v6);
		// No same-family candidate falls back to the first.
		assert_eq!(pick_local(&[v4], "[2001:db8::2]:1".parse().unwrap()), v4);
	}

	/// An agent on a fresh socket, which tokio reports unwritable until the runtime
	/// polls it, so every send waits in the outbox until the test yields.
	fn unpolled() -> Agent {
		let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
		socket.set_nonblocking(true).unwrap();
		let socket = Arc::new(UdpSocket::from_std(socket).unwrap());
		let local = socket.local_addr().unwrap();
		let socket = Socket::new(socket.clone(), vec![local], crate::ice::reader(socket)).unwrap();
		Agent::new(Rtc::builder().build(Instant::now().into_std()), socket).unwrap()
	}

	#[tokio::test]
	async fn outbox_is_bounded() {
		let mut agent = unpolled();
		let dst = agent.locals[0];
		for _ in 0..crate::ice::INBOX + 10 {
			agent.send(&[1; 100], dst);
		}
		assert_eq!(agent.outbox.len(), crate::ice::INBOX);
	}

	/// A peer that answers but never sends: DTLS gives up on its handshake before ICE
	/// connects, which reports as ICE never connecting rather than a DTLS failure.
	#[tokio::test(start_paused = true)]
	async fn a_handshake_giving_up_before_ice_connects_is_a_timeout() {
		let mut offerer = Rtc::builder().build(Instant::now().into_std());
		let mut api = offerer.sdp_api();
		api.add_media(
			str0m::media::MediaKind::Audio,
			str0m::media::Direction::SendOnly,
			None,
			None,
			None,
		);
		let (offer, pending) = api.apply().unwrap();
		let answer = Rtc::builder()
			.build(Instant::now().into_std())
			.sdp_api()
			.accept_offer(offer)
			.unwrap();
		offerer.sdp_api().accept_answer(pending, answer).unwrap();

		let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let local = socket.local_addr().unwrap();
		let socket = Socket::new(socket.clone(), vec![local], crate::ice::reader(socket)).unwrap();
		let mut agent = Agent::new(offerer, socket).unwrap();
		// Hold the deadline past the handshake's retransmissions, so DTLS
		// deterministically gives up first.
		let held = Instant::now().saturating_add(Duration::from_secs(3600));
		agent.phase = Phase::Establishing { started: held };

		let err = loop {
			match agent.next().await {
				Ok(_) => continue,
				Err(err) => break err,
			}
		};
		assert!(matches!(err, Error::Timeout), "{err:?}");
		assert!(Instant::now() < held, "the deadline fired, not DTLS");
		// The reason sticks: driving the agent again reports it again.
		let again = agent.next().await;
		assert!(matches!(again, Err(Error::Timeout)), "{again:?}");
	}

	#[test]
	fn mapped_addresses_round_trip() {
		let v4: SocketAddr = "1.2.3.4:5000".parse().unwrap();
		let mapped = to_family(v4, true);
		assert_eq!(mapped, "[::ffff:1.2.3.4]:5000".parse().unwrap());
		assert_eq!(canonical(mapped), v4);
		assert_eq!(to_family(v4, false), v4);
	}
}
