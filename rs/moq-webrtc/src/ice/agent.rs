use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;

use kio::clock::Instant;
use str0m::ice::{StunMessage, TransId};
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

/// How many of its latest binding requests an establishing agent remembers the
/// destination of, and how many addresses that answered one. str0m paces a new check
/// every 50 ms, so this spans the last few seconds of checks and their retransmits; an
/// answer to an older one only fails to confirm, and the address then waits for its
/// holder to end.
const CHECKS: usize = 64;

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
		/// Where each recent binding request went, newest last, at most [`CHECKS`].
		checks: VecDeque<(TransId, SocketAddr)>,
		/// The addresses that answered one of `checks` themselves, at most [`CHECKS`].
		answered: Vec<SocketAddr>,
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
	/// Close once connected with nothing due, which [`Agent::close`] asked for.
	closing: bool,
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
			phase: Phase::Establishing {
				started: now,
				checks: VecDeque::new(),
				answered: Vec::new(),
			},
			closing: false,
		};
		// str0m's ICE has no clock, and asks for no timeout, until its first one. Without
		// it a side that hears no checks, such as one facing an ICE-lite peer, would never
		// check the candidates signaling gave it. Draining on drop sends what this
		// and signaling produced.
		let result = agent.rtc_mut().handle_input(Input::Timeout(now.into_std()));
		if let Err(err) = result {
			agent.fail(Error::rtc(err));
		}
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

	/// An event str0m already surfaced, without driving the connection.
	pub fn queued(&mut self) -> Option<Event> {
		self.events.pop_front()
	}

	/// Close the connection with an RTCP BYE and a DTLS close_notify once it has sent
	/// everything written to it, after which [`Agent::next`] reports [`Error::Closed`].
	///
	/// str0m packetizes a written frame only on a timeout, and holds media until DTLS and
	/// SRTP are up, so the close waits for a connection with nothing due. A peer that never
	/// connects still ends with [`Error::Timeout`].
	pub fn close(&mut self) {
		self.closing = true;
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
			// Sent before reporting an end too, which may have queued a close_notify.
			while let Some((data, dst)) = self.outbox.front() {
				match self.socket.send_to(data, *dst).await {
					Ok(_) => {}
					Err(err) => tracing::warn!(%err, dst = %dst, "send failed"),
				}
				self.outbox.pop_front();
			}
			if let Phase::Failed(err) = &self.phase {
				return Err(err.clone());
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
			// Once SRTP is up with nothing due, str0m has packetized and sent every frame
			// written to it.
			if deadline > now && self.rtc.is_connected() && std::mem::take(&mut self.closing) {
				let result = self.rtc_mut().close();
				if let Err(err) = result {
					self.fail(Error::rtc(err));
				}
				continue;
			}
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
			Phase::Establishing { started, .. } => Some(started.saturating_add(ESTABLISH_TIMEOUT)),
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
				Output::Transmit(transmit) => {
					// Only a shared socket's route needs to know where checks went.
					if self.inbound.route.is_some()
						&& let Phase::Establishing { checks, .. } = &mut self.phase
						&& let Some(check) = stun(&transmit.contents).filter(|m| m.is_binding_request())
					{
						if checks.len() == CHECKS {
							checks.pop_front();
						}
						checks.push_back((check.trans_id(), transmit.destination));
					}
					self.send(&transmit.contents, transmit.destination)
				}
				Output::Event(event) => {
					if let str0m::Event::IceConnectionStateChange(state) = &event {
						tracing::debug!(?state, "ice state");
						if *state == IceConnectionState::Disconnected {
							return self.fail(Error::Closed);
						}
						if state.is_connected() {
							// Take each claimed address the peer proved it is at.
							if let Phase::Establishing { answered, .. } = &self.phase
								&& let Some(route) = &self.inbound.route
							{
								answered.iter().for_each(|addr| route.confirm(*addr));
							}
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
		// Before str0m sees it, since the answer may be what connects ICE.
		if self.inbound.route.is_some()
			&& let Phase::Establishing { checks, answered, .. } = &mut self.phase
			&& answered.len() < CHECKS
			&& !answered.contains(&src)
			&& answers_check(checks, data, src)
		{
			answered.push(src);
		}
		let result = self
			.rtc_mut()
			.handle_input(Input::Receive(Instant::now().into_std(), receive));
		if let Err(err) = result {
			self.fail(Error::rtc(err));
		}
	}
}

/// Whether a datagram from `src` answers a check this agent sent to `src`, which proves
/// the peer is there: only `src` saw that check's transaction ID. That is the check RFC 8445
/// §7.2.5.2.1 asks for and str0m skips, pairing a response with its check by transaction
/// ID alone, so a peer could answer its own check from a spoofed source.
///
/// Once ICE connects, the agent confirms every claimed address that answered, never the
/// source of whatever datagram connected it: once any pair is valid, str0m may report the
/// connection while handling another datagram, such as a spoofed request after one switched
/// its role to controlling.
fn answers_check(checks: &VecDeque<(TransId, SocketAddr)>, data: &[u8], src: SocketAddr) -> bool {
	let Some(message) = stun(data) else {
		return false;
	};
	let id = message.trans_id();
	message.is_successful_binding_response() && checks.iter().any(|check| *check == (id, src))
}

/// Parse a datagram as STUN, which alone starts with 0..=3 (RFC 7983).
fn stun(data: &[u8]) -> Option<StunMessage<'_>> {
	if data.first().is_none_or(|first| *first > 3) {
		return None;
	}
	StunMessage::parse(data).ok()
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
		Agent::new(crate::session::rtc_config().build(Instant::now().into_std()), socket).unwrap()
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
		let mut offerer = crate::session::rtc_config().build(Instant::now().into_std());
		let mut api = offerer.sdp_api();
		api.add_media(
			str0m::media::MediaKind::Audio,
			str0m::media::Direction::SendOnly,
			None,
			None,
			None,
		);
		let (offer, pending) = api.apply().unwrap();
		let answer = crate::session::rtc_config()
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
		agent.phase = Phase::Establishing {
			started: held,
			checks: VecDeque::new(),
			answered: Vec::new(),
		};

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

	/// Facing an ICE-lite offerer, which never sends a check, the answerer checks the
	/// offer's candidate on its own.
	#[tokio::test]
	async fn an_ice_lite_offerers_candidate_is_checked_unprompted() {
		let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
		let now = Instant::now().into_std();
		let mut offerer = crate::session::rtc_config().set_ice_lite(true).build(now);
		offerer.add_local_candidate(str0m::Candidate::host(peer.local_addr().unwrap(), "udp").unwrap());
		let mut api = offerer.sdp_api();
		api.add_media(
			str0m::media::MediaKind::Audio,
			str0m::media::Direction::SendOnly,
			None,
			None,
			None,
		);
		let (offer, _) = api.apply().unwrap();
		// An offerer taking the DTLS client role, with no SSRCs declared, leaves the
		// answerer no DTLS or media timer that would start ICE's clock instead.
		let offer: String = offer
			.to_sdp_string()
			.replace("a=setup:actpass", "a=setup:active")
			.split_inclusive("\r\n")
			.filter(|line| !line.starts_with("a=ssrc"))
			.collect();
		let offer = str0m::change::SdpOffer::from_sdp_string(&offer).unwrap();

		let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let local = socket.local_addr().unwrap();
		let mut answerer = crate::session::rtc_config().build(now);
		answerer.add_local_candidate(str0m::Candidate::host(local, "udp").unwrap());
		answerer.sdp_api().accept_offer(offer).unwrap();
		let socket = Socket::new(socket.clone(), vec![local], crate::ice::reader(socket)).unwrap();
		let mut agent = Agent::new(answerer, socket).unwrap();

		let mut buf = [0u8; 1500];
		tokio::select! {
			received = peer.recv_from(&mut buf) => {
				let (len, src) = received.unwrap();
				assert_eq!(src, local);
				// A STUN Binding request.
				assert!(len >= 20 && buf[..2] == [0x00, 0x01], "{:?}", &buf[..len]);
			}
			err = async { loop { if let Err(err) = agent.next().await { break err } } } => {
				panic!("the agent ended without a check: {err:?}");
			}
		}
	}

	/// A close sends every frame written before it: str0m packetizes a frame only on a
	/// timeout, so closing at once would drop what it still queued.
	#[tokio::test]
	async fn a_close_sends_what_was_written_first() {
		use str0m::media::{Direction, Frequency, MediaKind, MediaTime};
		const FRAMES: u64 = 50;

		let now = Instant::now().into_std();
		let mut sender = crate::session::rtc_config().build(now);
		let mut receiver = crate::session::rtc_config().build(now);
		// Candidates go into the SDP, so bind before the exchange.
		let sender_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let receiver_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		for (rtc, socket) in [(&mut sender, &sender_socket), (&mut receiver, &receiver_socket)] {
			let local = socket.local_addr().unwrap();
			rtc.add_local_candidate(str0m::Candidate::host(local, "udp").unwrap());
		}
		let mut api = sender.sdp_api();
		let mid = api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
		let (offer, pending) = api.apply().unwrap();
		let answer = receiver.sdp_api().accept_offer(offer).unwrap();
		sender.sdp_api().accept_answer(pending, answer).unwrap();
		let agent = |rtc, socket: Arc<UdpSocket>| {
			let local = socket.local_addr().unwrap();
			Agent::new(
				rtc,
				Socket::new(socket.clone(), vec![local], crate::ice::reader(socket)).unwrap(),
			)
			.unwrap()
		};
		let mut sender = agent(sender, sender_socket);
		let mut receiver = agent(receiver, receiver_socket);

		while !(sender.rtc().is_connected() && receiver.rtc().is_connected()) {
			tokio::select! {
				event = sender.next() => { event.unwrap(); }
				event = receiver.next() => { event.unwrap(); }
			}
		}

		// Every frame written back to back, each through its own guard, which drains it as
		// str0m requires before the next write, then the close.
		let pt = (sender.rtc_mut().writer(mid).unwrap().payload_params().next().unwrap()).pt();
		for n in 0..FRAMES {
			let time = MediaTime::new(960 * n, Frequency::FORTY_EIGHT_KHZ);
			(sender.rtc_mut().writer(mid).unwrap())
				.write(pt, now, time, vec![0xfc, n as u8])
				.unwrap();
		}
		sender.close();

		let mut received = 0;
		let mut sending = true;
		loop {
			tokio::select! {
				event = sender.next(), if sending => match event {
					Ok(_) => {}
					Err(Error::Closed) => sending = false,
					Err(err) => panic!("the sender failed: {err:?}"),
				},
				event = receiver.next() => match event {
					Ok(Event::MediaData(_)) => received += 1,
					Ok(_) => {}
					// The sender's close_notify ends the receiver.
					Err(Error::Closed) => break,
					Err(err) => panic!("the receiver failed: {err:?}"),
				},
			}
		}
		assert_eq!(received, FRAMES, "frames written before the close were lost");
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
