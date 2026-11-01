use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;

use kio::clock::Instant;
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, IceConnectionState, Input, Output, Rtc};
use stun_types::attribute::XorMappedAddress;
use stun_types::message::{BINDING, Message, MessageClass, MessageWriteVec, TransactionId};
use stun_types::prelude::*;
use tokio::net::UdpSocket;

use crate::{Inbound, Socket, Wire};

/// Backstop deadline for reaching a connected ICE state, covering the one case
/// str0m's agent deliberately never times out: a peer that provides no remote
/// candidates and sends nothing. str0m ends a connection whose candidate pairs were
/// tried and exhausted (after ~21s at its defaults), but with no remote candidates
/// it treats the session as still possible forever, since trickle ICE could add
/// one. Set above str0m's own exhaustion so a slow but real peer is not clipped.
///
/// It counts from when the peer could hold every local candidate: once gathering
/// ends, so a caller that sends its description only after the last candidate is
/// not clipped by an unreachable STUN server's retransmissions, and for an
/// [`Agent::awaiting_answer`], once the answer applies.
const ESTABLISH_TIMEOUT: Duration = Duration::from_secs(30);

/// RFC 8489 section 6.2.1: the first retransmission timeout of a STUN request.
const STUN_RTO: Duration = Duration::from_millis(500);

/// RFC 8489 section 6.2.1: how many times a STUN request is sent (`Rc`).
const STUN_SENDS: u32 = 7;

/// RFC 8489 section 6.2.1: how long to wait after the last send, in RTOs (`Rm`).
const STUN_LAST_WAIT: u32 = 16;

/// Why an [`Agent`] stopped.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
	/// The peer disconnected, str0m closed the connection, or the inbound queue ended.
	#[error("peer connection closed")]
	Closed,

	/// ICE never connected before the establishment deadline, or before the DTLS
	/// handshake gave up waiting for it.
	#[error("ICE did not connect in time")]
	Timeout,

	/// str0m failed (SDP, DTLS, SCTP, or ICE state).
	#[error("rtc error: {0}")]
	Rtc(String),
}

impl Error {
	fn rtc(err: impl std::fmt::Display) -> Self {
		Self::Rtc(err.to_string())
	}
}

/// What [`Agent::next`] surfaced.
#[derive(Debug)]
#[non_exhaustive]
#[expect(
	clippy::large_enum_variant,
	reason = "moved once per poll; boxing would allocate for every data channel message"
)]
pub enum Event {
	/// An event from str0m. ICE disconnection is reported as [`Error::Closed`] instead.
	Rtc(str0m::Event),
	/// A server-reflexive candidate, already added locally, to signal to the peer.
	Candidate(Candidate),
	/// Every STUN server answered or gave up: no more local candidates follow.
	Gathered,
}

/// A STUN Binding request in flight to one server.
struct Query {
	server: SocketAddr,
	/// The host candidate the request leaves from, which a reply's mapping is the reflexive form of.
	base: SocketAddr,
	id: TransactionId,
	request: Vec<u8>,
	sends: u32,
	rto: Duration,
	/// When to retransmit, or give up after the last send.
	deadline: Instant,
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
	stun: Vec<Query>,
	/// When every STUN query answered or gave up, once [`Event::Gathered`] went out.
	gathered: Option<Instant>,
	events: VecDeque<Event>,
	/// Why the connection ended, found while draining and reported by the next [`Agent::next`].
	failed: Option<Error>,
	/// When str0m next wants a timeout, as of the last drain.
	timeout: Instant,
	/// Datagrams the socket refused for lack of buffer space, sent first next time.
	outbox: VecDeque<(Vec<u8>, SocketAddr)>,
	/// When the [`ESTABLISH_TIMEOUT`] started, or `None` while an offer awaits its answer.
	started: Option<Instant>,
	connected: bool,
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
			stun: Vec::new(),
			gathered: None,
			events: VecDeque::new(),
			failed: None,
			timeout: now,
			outbox: VecDeque::new(),
			started: Some(now),
			connected: false,
		};
		// Signaling already changed the connection.
		agent.drain();
		Ok(agent)
	}

	/// Ask each STUN server for this socket's public mapping, offering what comes
	/// back as a server-reflexive candidate. Servers of a family with no host
	/// candidate are skipped.
	pub fn with_stun(mut self, servers: &[SocketAddr]) -> Self {
		let now = Instant::now();
		for &server in servers {
			let server = canonical(server);
			let Some(base) = self.locals.iter().find(|l| l.is_ipv4() == server.is_ipv4()).copied() else {
				tracing::debug!(%server, "no host candidate of this family; skipping STUN server");
				continue;
			};
			let request = Message::builder_request(BINDING, MessageWriteVec::new());
			let id = request.transaction_id();
			self.stun.push(Query {
				server,
				base,
				id,
				request: request.finish(),
				sends: 0,
				rto: STUN_RTO,
				deadline: now,
			});
		}
		self
	}

	/// The connection being driven, read only.
	pub fn rtc(&self) -> &Rtc {
		&self.rtc
	}

	/// Hold off the deadline for ICE to connect until the first [`Agent::rtc_mut`],
	/// for an offer whose answer has yet to arrive: signaling time is the caller's.
	pub fn awaiting_answer(mut self) -> Self {
		self.started = None;
		self
	}

	/// Change the connection through a guard that drains what the change produced
	/// when dropped: transmits are sent, and events wait for [`Agent::next`].
	pub fn rtc_mut(&mut self) -> RtcMut<'_> {
		self.started.get_or_insert_with(Instant::now);
		self.change()
	}

	/// [`Agent::rtc_mut`] for the agent's own changes, which never start the deadline.
	fn change(&mut self) -> RtcMut<'_> {
		RtcMut(self)
	}

	/// The counters of the bytes exchanged with the peer.
	pub fn wire(&self) -> &Wire {
		&self.inbound.wire
	}

	/// Drive the connection until it surfaces an [`Event`].
	///
	/// Cancel safe: the connection is always drained between calls, and a datagram
	/// the socket could not take yet is kept and sent first on the next call.
	pub async fn next(&mut self) -> Result<Event, Error> {
		loop {
			if let Some(event) = self.events.pop_front() {
				return Ok(event);
			}
			if let Some(err) = self.failed.take() {
				return Err(err);
			}
			if self.gathered.is_none() && self.stun.is_empty() {
				self.gathered = Some(Instant::now());
				return Ok(Event::Gathered);
			}

			while let Some((data, dst)) = self.outbox.front() {
				match self.socket.send_to(data, *dst).await {
					Ok(len) => self.inbound.wire.sent(len),
					Err(err) => tracing::warn!(%err, dst = %dst, "send failed"),
				}
				self.outbox.pop_front();
			}

			// A dead Rtc (DTLS failure, explicit disconnect) reports a never-firing
			// timeout instead of an error, which would park this loop forever.
			if !self.rtc.is_alive() {
				return Err(Error::Closed);
			}
			let establish = self.establish();
			if establish.is_some_and(|at| at <= Instant::now()) {
				return Err(Error::Timeout);
			}

			let deadline = self.stun.iter().map(|q| q.deadline).chain(establish);
			let deadline = deadline.fold(self.timeout, Instant::min);

			let now = Instant::now();
			if deadline > now {
				tokio::select! {
					packet = self.inbound.packets.recv() => {
						let Some((data, src)) = packet else {
							// Every sender dropped: nothing more will arrive.
							return Err(Error::Closed);
						};
						self.receive(&data, src);
						continue;
					}
					_ = kio::clock::sleep_until(deadline) => {}
				}
			}

			let now = Instant::now();
			let result = self.change().handle_input(Input::Timeout(now.into_std()));
			if let Err(err) = result {
				self.fail(Error::rtc(err));
			}
			self.retransmit(now);
		}
	}

	/// When [`ESTABLISH_TIMEOUT`] expires, or `None` while it is held or ICE connected.
	fn establish(&self) -> Option<Instant> {
		match (self.started, self.gathered) {
			(Some(started), Some(gathered)) if !self.connected => Some(started.max(gathered) + ESTABLISH_TIMEOUT),
			_ => None,
		}
	}

	/// Poll str0m until it asks for a timeout: transmits are sent, events queued.
	fn drain(&mut self) {
		while self.failed.is_none() {
			let output = match self.rtc.poll_output() {
				Ok(output) => output,
				// Before ICE connects no DTLS record can reach the peer, so a DTLS failure
				// is its handshake giving up on retransmitting into the void. Its flights
				// (~31s, randomly jittered) race the establishment deadline, so report
				// either as ICE never connecting.
				Err(str0m::RtcError::Dtls(_)) if !self.connected => return self.fail(Error::Timeout),
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
						self.connected |= state.is_connected();
					}
					self.events.push_back(Event::Rtc(event));
				}
			}
		}
	}

	/// Keep the first reason the connection ended.
	fn fail(&mut self, err: Error) {
		self.failed.get_or_insert(err);
	}

	/// Send without waiting, keeping the datagram for later when the socket is full.
	fn send(&mut self, data: &[u8], dst: SocketAddr) {
		let dst = to_family(dst, self.socket_v6);
		match self.socket.try_send_to(data, dst) {
			Ok(len) => self.inbound.wire.sent(len),
			// Changes can keep coming while the socket stays full, so the backlog is
			// bounded like the inbound queue; ICE, SCTP, and RTP recover the loss.
			Err(err) if err.kind() == std::io::ErrorKind::WouldBlock && self.outbox.len() < crate::INBOX => {
				self.outbox.push_back((data.to_vec(), dst))
			}
			Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
				tracing::debug!(%dst, len = data.len(), "dropped a datagram: the socket is full")
			}
			Err(err) => tracing::warn!(%err, %dst, "send failed"),
		}
	}

	fn receive(&mut self, data: &[u8], src: SocketAddr) {
		if self.stun_reply(data, src) {
			return;
		}

		let local = pick_local(&self.locals, src);
		// Anything can arrive on a socket a server-reflexive candidate exposes, so a
		// datagram that is not STUN, DTLS, or RTP is noise, not a failure.
		let Ok(receive) = Receive::new(Protocol::Udp, src, local, data) else {
			tracing::debug!(%src, len = data.len(), "dropped unrecognized datagram");
			return;
		};
		let result = self
			.change()
			.handle_input(Input::Receive(Instant::now().into_std(), receive));
		if let Err(err) = result {
			self.fail(Error::rtc(err));
		}
	}

	/// Consume a reply to one of our STUN queries, returning false for anything else.
	fn stun_reply(&mut self, data: &[u8], src: SocketAddr) -> bool {
		if self.stun.is_empty() {
			return false;
		}
		let Ok(reply) = Message::from_bytes(data) else {
			return false;
		};
		let id = reply.transaction_id();
		let Some(index) = self.stun.iter().position(|q| q.id == id && q.server == src) else {
			return false;
		};
		let query = self.stun.swap_remove(index);

		if !reply.has_method(BINDING) || !reply.has_class(MessageClass::Success) {
			tracing::debug!(server = %query.server, "STUN server refused the binding request");
			return true;
		}
		let Ok(mapped) = reply.attribute::<XorMappedAddress>() else {
			tracing::debug!(server = %query.server, "STUN reply carries no mapped address");
			return true;
		};

		let mapped = canonical(mapped.addr(id));
		match Candidate::server_reflexive(mapped, query.base, "udp") {
			// None: redundant with a candidate the agent already has, as when there is no NAT.
			Ok(candidate) => {
				let added = self.change().add_local_candidate(candidate).cloned();
				if let Some(candidate) = added {
					self.events.push_back(Event::Candidate(candidate));
				}
			}
			Err(err) => tracing::debug!(%err, %mapped, "unusable server-reflexive address"),
		}
		true
	}

	/// Send each STUN request that is due, dropping those out of attempts.
	fn retransmit(&mut self, now: Instant) {
		let mut due = Vec::new();
		self.stun.retain_mut(|query| {
			if query.deadline > now {
				return true;
			}
			if query.sends == STUN_SENDS {
				tracing::debug!(server = %query.server, "STUN server did not answer");
				return false;
			}
			query.sends += 1;
			query.deadline = now
				+ match query.sends {
					STUN_SENDS => STUN_RTO * STUN_LAST_WAIT,
					_ => query.rto,
				};
			query.rto *= 2;
			due.push((query.request.clone(), query.server));
			true
		});
		for (request, server) in due {
			self.send(&request, server);
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

	#[tokio::test]
	async fn wire_counts_only_accepted_payload() {
		let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
		let local = socket.local_addr().unwrap();
		let inbound = crate::reader(socket.clone());
		let wire = inbound.wire.clone();
		let socket = Socket::new(socket, vec![local], inbound).unwrap();
		let mut agent = Agent::new(Rtc::builder().build(Instant::now().into_std()), socket).unwrap();

		// Until tokio has seen the socket writable, a send waits in the outbox.
		agent.socket.writable().await.unwrap();
		agent.send(&[42; 73], peer.local_addr().unwrap());
		// UDP refuses an oversized datagram, which is never counted as sent.
		agent.send(&vec![0; 65_536], peer.local_addr().unwrap());
		assert_eq!(wire.stats(), crate::Stats { tx: 73, rx: 0 });

		peer.send_to(&[7; 11], local).await.unwrap();
		let (data, _) = agent.inbound.packets.recv().await.unwrap();
		assert_eq!(data.len(), 11);
		drop(agent);
		assert_eq!(wire.stats(), crate::Stats { tx: 73, rx: 11 });
	}

	/// An agent on a fresh socket, which tokio reports unwritable until the runtime
	/// polls it, so every send waits in the outbox until the test yields.
	fn unpolled() -> Agent {
		let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
		socket.set_nonblocking(true).unwrap();
		let socket = Arc::new(UdpSocket::from_std(socket).unwrap());
		let local = socket.local_addr().unwrap();
		let socket = Socket::new(socket.clone(), vec![local], crate::reader(socket)).unwrap();
		Agent::new(Rtc::builder().build(Instant::now().into_std()), socket).unwrap()
	}

	#[tokio::test]
	async fn outbox_is_bounded() {
		let mut agent = unpolled();
		let dst = agent.locals[0];
		for _ in 0..crate::INBOX + 10 {
			agent.send(&[1; 100], dst);
		}
		assert_eq!(agent.outbox.len(), crate::INBOX);
	}

	#[tokio::test]
	async fn awaiting_answer_holds_the_deadline() {
		let mut agent = unpolled().awaiting_answer();
		assert_eq!(agent.started, None);
		// The agent's own changes, such as timeouts and STUN, leave it held.
		drop(agent.change());
		assert_eq!(agent.started, None);
		drop(agent.rtc_mut());
		assert!(agent.started.is_some());
	}

	#[tokio::test]
	async fn gathering_holds_the_deadline() {
		let mut agent = unpolled().with_stun(&["127.0.0.1:3478".parse().unwrap()]);
		// An unreachable STUN server retransmits for longer than the deadline, so the
		// deadline waits for gathering to end.
		assert_eq!(agent.establish(), None);
		agent.stun.clear();
		assert!(matches!(agent.next().await, Ok(Event::Gathered)));
		let gathered = agent.gathered.unwrap();
		assert_eq!(agent.establish(), Some(gathered + ESTABLISH_TIMEOUT));
	}

	/// A peer that answers but never sends: DTLS gives up on its handshake before ICE
	/// connects, which reports as ICE never connecting rather than a DTLS failure.
	#[tokio::test(start_paused = true)]
	async fn a_handshake_giving_up_before_ice_connects_is_a_timeout() {
		let mut offerer = Rtc::builder().build(Instant::now().into_std());
		let mut api = offerer.sdp_api();
		api.add_channel("test".into());
		let (offer, pending) = api.apply().unwrap();
		let answer = Rtc::builder()
			.build(Instant::now().into_std())
			.sdp_api()
			.accept_offer(offer)
			.unwrap();
		offerer.sdp_api().accept_answer(pending, answer).unwrap();

		let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
		let local = socket.local_addr().unwrap();
		let socket = Socket::new(socket.clone(), vec![local], crate::reader(socket)).unwrap();
		// A STUN server that never answers holds the deadline past the handshake's
		// retransmissions, so DTLS deterministically gives up first.
		let stun = UdpSocket::bind("127.0.0.1:0").await.unwrap();
		let mut agent = Agent::new(offerer, socket)
			.unwrap()
			.with_stun(&[stun.local_addr().unwrap()]);

		let err = loop {
			match agent.next().await {
				Ok(_) => continue,
				Err(err) => break err,
			}
		};
		assert!(matches!(err, Error::Timeout), "{err:?}");
		assert_eq!(agent.establish(), None, "the deadline had yet to start");
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
