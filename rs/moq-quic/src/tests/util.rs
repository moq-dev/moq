use std::{
	cmp,
	collections::{BTreeMap, HashSet, VecDeque},
	env,
	io::{self, Write},
	mem,
	net::{Ipv6Addr, SocketAddr, UdpSocket},
	str,
	sync::{Arc, LazyLock, Mutex},
};

use assert_matches::assert_matches;
use bytes::BytesMut;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use rustls::{
	KeyLogFile,
	client::WebPkiServerVerifier,
	pki_types::{CertificateDer, PrivateKeyDer},
};
use tracing::{info_span, trace};

use super::crypto::rustls::{QuicClientConfig, QuicServerConfig, configured_provider};
use super::*;
use crate::{Duration, Instant, entropy};

pub(super) const DEFAULT_MTU: usize = 1452;

pub(super) struct Pair {
	pub(super) server: TestEndpoint,
	pub(super) client: TestEndpoint,
	/// Start time
	epoch: Instant,
	/// Current time
	pub(super) time: Instant,
	/// Simulates the maximum size allowed for UDP payloads by the link (packets exceeding this size will be dropped)
	pub(super) mtu: usize,
	/// Simulates explicit congestion notification
	pub(super) congestion_experienced: bool,
	/// Replaces the ECN codepoint of every delivered datagram after any marking, as a path that
	/// bleaches the field (`Some(None)`) or re-marks it
	pub(super) rewrite_ecn: Option<Option<EcnCodepoint>>,
	/// The client-to-server direction of the path
	pub(super) to_server: Link,
	/// The server-to-client direction of the path
	pub(super) to_client: Link,
	// One-way
	pub(super) latency: Duration,
	/// Number of spin bit flips
	pub(super) spins: u64,
	last_spin: bool,
	/// Draws every [`Link`] impairment from the test thread's seed, as the endpoints draw their
	/// keys and connection IDs, so `entropy::reseed` before building a pair replays all of them
	rng: StdRng,
	/// The next port [`Self::next_port`] hands out
	port: u16,
}

impl Pair {
	pub(super) fn default_with_deterministic_pns() -> Self {
		let mut cfg = server_config();
		let transport = transport::Config {
			deterministic_packet_numbers: true,
			..transport::Config::default()
		};
		cfg.transport = transport;
		Self::new(Default::default(), cfg)
	}

	pub(super) fn new(endpoint_config: endpoint::Config, server_config: server::Config) -> Self {
		let server = Endpoint::new(
			endpoint::Config {
				server: Some(server_config),
				..endpoint_config.clone()
			},
			true,
		)
		.unwrap();
		let client = Endpoint::new(endpoint_config, true).unwrap();

		Self::new_from_endpoint(client, server)
	}

	pub(super) fn new_from_endpoint(client: Endpoint, server: Endpoint) -> Self {
		let now = Instant::now();
		let mut pair = Self {
			server: TestEndpoint::new(server, SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 4433)),
			client: TestEndpoint::new(client, SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 44433)),
			epoch: now,
			time: now,
			mtu: DEFAULT_MTU,
			latency: Duration::ZERO,
			spins: 0,
			last_spin: false,
			congestion_experienced: false,
			rewrite_ecn: None,
			to_server: Link::default(),
			to_client: Link::default(),
			rng: StdRng::from_rng(&mut entropy::rng()),
			port: 44434,
		};
		pair.server.bind();
		pair.client.bind();
		pair
	}

	/// A client port no address of this pair uses yet, as for a migration
	pub(super) fn next_port(&mut self) -> u16 {
		self.port += 1;
		self.port - 1
	}

	/// Returns whether the connection is not idle
	pub(super) fn step(&mut self) -> bool {
		self.drive_client();
		self.drive_server();
		if self.client.is_idle() && self.server.is_idle() {
			return false;
		}

		let client_t = self.client.next_wakeup();
		let server_t = self.server.next_wakeup();
		match min_opt(client_t, server_t) {
			Some(t) if Some(t) == client_t => {
				if t != self.time {
					self.time = self.time.max(t);
					trace!("advancing to {:?} for client", self.time - self.epoch);
				}
				true
			}
			Some(t) if Some(t) == server_t => {
				if t != self.time {
					self.time = self.time.max(t);
					trace!("advancing to {:?} for server", self.time - self.epoch);
				}
				true
			}
			Some(_) => unreachable!(),
			None => false,
		}
	}

	/// Advance time until both connections are idle
	pub(super) fn drive(&mut self) {
		while self.step() {}
	}

	/// Advance time until both connections are idle, or after 100 steps have been executed
	///
	/// Returns true if the amount of steps exceeds the bounds, because the connections never became
	/// idle
	pub(super) fn drive_bounded(&mut self) -> bool {
		for _ in 0..100 {
			if !self.step() {
				return false;
			}
		}

		true
	}

	pub(super) fn drive_client(&mut self) {
		let span = info_span!("client");
		let _guard = span.enter();
		self.client.drive(self.time, self.server.addr);
		for (packet, buffer) in self.client.outbound.drain(..) {
			let packet_size = packet_size(&packet, &buffer);
			if packet_size > self.mtu {
				info!(packet_size, "dropping packet (max size exceeded)");
				continue;
			}
			if buffer[0] & packet::LONG_HEADER_FORM == 0 {
				let spin = buffer[0] & packet::SPIN_BIT != 0;
				self.spins += (spin == self.last_spin) as u64;
				self.last_spin = spin;
			}
			if let Some(socket) = &self.client.socket {
				socket.send_to(&buffer, packet.remote).unwrap();
			}
			if self.server.addr == packet.remote {
				let (marked, arrivals) = self
					.to_server
					.transmit(&mut self.rng, self.time, self.latency, buffer.len());
				let ecn = self
					.rewrite_ecn
					.unwrap_or_else(|| set_congestion_experienced(packet.ecn, self.congestion_experienced || marked));
				for arrival in arrivals {
					self.server.receive(arrival, ecn, &buffer);
				}
			}
		}
	}

	pub(super) fn drive_server(&mut self) {
		let span = info_span!("server");
		let _guard = span.enter();
		self.server.drive(self.time, self.client.addr);
		for (packet, buffer) in self.server.outbound.drain(..) {
			let packet_size = packet_size(&packet, &buffer);
			if packet_size > self.mtu {
				info!(packet_size, "dropping packet (max size exceeded)");
				continue;
			}
			if let Some(socket) = &self.server.socket {
				socket.send_to(&buffer, packet.remote).unwrap();
			}
			if self.client.addr == packet.remote {
				let (marked, arrivals) = self
					.to_client
					.transmit(&mut self.rng, self.time, self.latency, buffer.len());
				let ecn = self
					.rewrite_ecn
					.unwrap_or_else(|| set_congestion_experienced(packet.ecn, self.congestion_experienced || marked));
				for arrival in arrivals {
					self.client.receive(arrival, ecn, &buffer);
				}
			}
		}
	}

	pub(super) fn connect(&mut self) -> (ConnectionHandle, ConnectionHandle) {
		self.connect_with(client_config())
	}

	pub(super) fn connect_with(&mut self, config: client::Config) -> (ConnectionHandle, ConnectionHandle) {
		info!("connecting");
		let client_ch = self.begin_connect(config);
		self.drive();
		let server_ch = self.server.assert_accept();
		self.finish_connect(client_ch, server_ch);
		(client_ch, server_ch)
	}

	/// Just start connecting the client
	pub(super) fn begin_connect(&mut self, config: client::Config) -> ConnectionHandle {
		let span = info_span!("client");
		let _guard = span.enter();
		let (client_ch, client_conn) = self
			.client
			.connect(
				self.time,
				Dial {
					config: &config,
					remote: self.server.addr,
					server_name: "localhost",
				},
			)
			.unwrap();
		self.client.connections.insert(client_ch, client_conn);
		client_ch
	}

	pub(super) fn finish_connect(&mut self, client_ch: ConnectionHandle, server_ch: ConnectionHandle) {
		assert_matches!(self.client_conn_mut(client_ch).poll(), Some(Event::HandshakeDataReady));
		assert_matches!(self.client_conn_mut(client_ch).poll(), Some(Event::Connected));
		assert_matches!(self.server_conn_mut(server_ch).poll(), Some(Event::HandshakeDataReady));
		assert_matches!(self.server_conn_mut(server_ch).poll(), Some(Event::HandshakeConfirmed));
		assert_matches!(self.server_conn_mut(server_ch).poll(), Some(Event::Connected));
		assert_matches!(self.client_conn_mut(client_ch).poll(), Some(Event::HandshakeConfirmed));
	}

	pub(super) fn client_conn_mut(&mut self, ch: ConnectionHandle) -> &mut Connection {
		self.client.connections.get_mut(&ch).unwrap()
	}

	pub(super) fn client_streams(&mut self, ch: ConnectionHandle) -> Streams<'_> {
		self.client_conn_mut(ch).streams()
	}

	pub(super) fn client_send(&mut self, ch: ConnectionHandle, s: StreamId) -> send::Stream<'_> {
		self.client_conn_mut(ch).send_stream(s)
	}

	pub(super) fn client_recv(&mut self, ch: ConnectionHandle, s: StreamId) -> recv::Stream<'_> {
		self.client_conn_mut(ch).recv_stream(s)
	}

	pub(super) fn client_datagrams(&mut self, ch: ConnectionHandle) -> Datagrams<'_> {
		self.client_conn_mut(ch).datagrams()
	}

	pub(super) fn server_conn_mut(&mut self, ch: ConnectionHandle) -> &mut Connection {
		self.server.connections.get_mut(&ch).unwrap()
	}

	pub(super) fn server_streams(&mut self, ch: ConnectionHandle) -> Streams<'_> {
		self.server_conn_mut(ch).streams()
	}

	pub(super) fn server_send(&mut self, ch: ConnectionHandle, s: StreamId) -> send::Stream<'_> {
		self.server_conn_mut(ch).send_stream(s)
	}

	pub(super) fn server_recv(&mut self, ch: ConnectionHandle, s: StreamId) -> recv::Stream<'_> {
		self.server_conn_mut(ch).recv_stream(s)
	}

	pub(super) fn server_datagrams(&mut self, ch: ConnectionHandle) -> Datagrams<'_> {
		self.server_conn_mut(ch).datagrams()
	}
}

impl Default for Pair {
	fn default() -> Self {
		Self::new(Default::default(), server_config())
	}
}

/// A rate-limited FIFO queue, as at a bottleneck router
#[derive(Debug, Clone)]
pub(super) struct Bottleneck {
	time_per_byte: Duration,
	/// The bytes the queue holds before it tail-drops
	buffer_size: u32,
	/// The queueing delay past which packets are tail-dropped
	max_queue: Duration,
	/// Whether packets are marked CE once the queue is half full, as a classic AQM does
	marks_ce: bool,
	/// When the link finishes serving everything queued so far
	busy_until: Option<Instant>,
	pub(super) stats: QueueStats,
}

impl Bottleneck {
	/// A link serving `bytes_per_second`, queueing up to `buffer_size` bytes
	pub(super) fn new(bytes_per_second: u64, buffer_size: u32, marks_ce: bool) -> Self {
		let time_per_byte = Duration::from_nanos(1_000_000_000 / bytes_per_second);
		Self {
			time_per_byte,
			buffer_size,
			max_queue: buffer_size * time_per_byte,
			marks_ce,
			busy_until: None,
			stats: QueueStats::default(),
		}
	}

	/// Serve at `bytes_per_second` from now on, keeping the queue's size in bytes
	///
	/// Packets already queued still leave when they were due, as if the link stepped once they
	/// had been served.
	pub(super) fn set_rate(&mut self, bytes_per_second: u64) {
		self.time_per_byte = Duration::from_nanos(1_000_000_000 / bytes_per_second);
		self.max_queue = self.buffer_size * self.time_per_byte;
	}

	/// Queues `bytes` arriving at `now`, returning when they leave the link and whether they
	/// were marked CE, or `None` if the full queue dropped them
	fn enqueue(&mut self, now: Instant, bytes: usize) -> Option<(Instant, bool)> {
		let busy_until = self.busy_until.unwrap_or(now).max(now);
		let delay = busy_until - now;
		if delay > self.max_queue {
			self.stats.dropped += 1;
			return None;
		}
		let service = bytes as u32 * self.time_per_byte;
		self.stats.queued += 1;
		self.stats.total_delay += delay;
		self.stats.max_delay = self.stats.max_delay.max(delay);
		if delay > self.max_queue / 2 {
			self.stats.congested += service;
		}

		let done = busy_until + service;
		self.busy_until = Some(done);
		let marked = self.marks_ce && done > now + self.max_queue / 2;
		self.stats.marked += marked as u64;
		Some((done, marked))
	}
}

/// One direction of the path: an optional bottleneck, then seeded loss, duplication, jitter,
/// and reordering
#[derive(Debug, Clone, Default)]
pub(super) struct Link {
	/// Queues datagrams at a rate-limited bottleneck, if set
	pub(super) bottleneck: Option<Bottleneck>,
	/// The probability a datagram leaving the bottleneck is lost
	pub(super) loss: f64,
	/// The probability a datagram arrives twice
	pub(super) duplicate: f64,
	/// Extra delay drawn uniformly up to this for each arrival, keeping arrivals in order
	pub(super) jitter: Duration,
	/// The probability an arrival is held back by up to `reorder_delay` more, so datagrams sent
	/// after it can overtake it
	pub(super) reorder: f64,
	/// The most a reordered arrival is held back
	pub(super) reorder_delay: Duration,
	/// The latest in-order arrival, which jitter never moves another before
	last_arrival: Option<Instant>,
}

impl Link {
	/// Sends `bytes` at `now`, returning whether the bottleneck marked them CE and when each copy
	/// that survives arrives
	fn transmit(&mut self, rng: &mut StdRng, now: Instant, latency: Duration, bytes: usize) -> (bool, Vec<Instant>) {
		let (sent, marked) = match &mut self.bottleneck {
			Some(bottleneck) => match bottleneck.enqueue(now, bytes) {
				Some(dequeued) => dequeued,
				None => return (false, Vec::new()),
			},
			None => (now, false),
		};
		if rng.random_bool(self.loss) {
			return (marked, Vec::new());
		}

		let copies = 1 + rng.random_bool(self.duplicate) as usize;
		let arrivals = (0..copies)
			.map(|_| {
				let arrival = sent + latency + rng.random_range(Duration::ZERO..=self.jitter);
				if rng.random_bool(self.reorder) {
					return arrival + rng.random_range(Duration::ZERO..=self.reorder_delay);
				}
				let arrival = self.last_arrival.map_or(arrival, |last| last.max(arrival));
				self.last_arrival = Some(arrival);
				arrival
			})
			.collect();
		(marked, arrivals)
	}
}

/// What a [`Bottleneck`] queue has seen
#[derive(Debug, Default, Clone)]
pub(super) struct QueueStats {
	/// Packets tail-dropped
	pub(super) dropped: u64,
	/// Packets marked CE
	pub(super) marked: u64,
	/// Packets queued for delivery
	pub(super) queued: u32,
	/// The queueing delay delivered packets met, summed
	pub(super) total_delay: Duration,
	/// The longest queueing delay a delivered packet met
	pub(super) max_delay: Duration,
	/// How long the link spent serving packets that queued past half full, the marking
	/// threshold, so how long the sender took to answer congestion, summed over the run
	pub(super) congested: Duration,
}

impl QueueStats {
	/// The mean queueing delay delivered packets met
	pub(super) fn mean_delay(&self) -> Duration {
		self.total_delay / self.queued.max(1)
	}
}

pub(super) struct TestEndpoint {
	pub(super) endpoint: Endpoint,
	pub(super) addr: SocketAddr,
	socket: Option<UdpSocket>,
	/// Each connection's next timer, as of its last poll
	timeouts: BTreeMap<ConnectionHandle, Instant>,
	pub(super) outbound: VecDeque<(Transmit, Bytes)>,
	delayed: VecDeque<(Transmit, Bytes)>,
	/// Datagrams in flight to this endpoint, in arrival order
	pub(super) inbound: VecDeque<(Instant, Option<EcnCodepoint>, BytesMut)>,
	accepted: Option<Result<ConnectionHandle, ConnectionError>>,
	// Ordered, so connections are driven in the same order every run
	pub(super) connections: BTreeMap<ConnectionHandle, Connection>,
	conn_events: BTreeMap<ConnectionHandle, VecDeque<ConnectionEvent>>,
	pub(super) captured_packets: Vec<Vec<u8>>,
	pub(super) capture_inbound_packets: bool,
	pub(super) handle_incoming: Box<dyn FnMut(&Incoming) -> IncomingConnectionBehavior>,
	pub(super) waiting_incoming: Vec<Incoming>,
	/// Every draining and drained endpoint event, in the order connections raised them
	pub(super) lifecycle: Vec<(ConnectionHandle, Lifecycle)>,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) enum Lifecycle {
	Draining,
	Drained,
}

#[derive(Debug, Copy, Clone)]
pub(super) enum IncomingConnectionBehavior {
	Accept,
	Reject,
	Retry,
	Wait,
}

pub(super) fn validate_incoming(incoming: &Incoming) -> IncomingConnectionBehavior {
	if incoming.remote_address_validated() {
		IncomingConnectionBehavior::Accept
	} else {
		IncomingConnectionBehavior::Retry
	}
}

impl TestEndpoint {
	fn new(endpoint: Endpoint, addr: SocketAddr) -> Self {
		Self {
			endpoint,
			addr,
			socket: None,
			timeouts: BTreeMap::new(),
			outbound: VecDeque::new(),
			delayed: VecDeque::new(),
			inbound: VecDeque::new(),
			accepted: None,
			connections: BTreeMap::new(),
			conn_events: BTreeMap::new(),
			captured_packets: Vec::new(),
			capture_inbound_packets: false,
			handle_incoming: Box::new(|_| IncomingConnectionBehavior::Accept),
			waiting_incoming: Vec::new(),
			lifecycle: Vec::new(),
		}
	}

	/// Mirrors the traffic onto a real socket when `SSLKEYLOGFILE` is set, so a capture can be
	/// decrypted
	///
	/// Every pair uses the same ports, so run one test at a time to capture.
	fn bind(&mut self) {
		if env::var_os("SSLKEYLOGFILE").is_none() {
			return;
		}
		let socket = UdpSocket::bind(self.addr).expect("failed to bind UDP socket; capture with --test-threads=1");
		socket.set_read_timeout(Some(Duration::from_millis(10))).unwrap();
		self.socket = Some(socket);
	}

	/// Queues a datagram that arrives at `at`, behind any that arrive no later
	fn receive(&mut self, at: Instant, ecn: Option<EcnCodepoint>, datagram: &[u8]) {
		let index = self.inbound.partition_point(|(arrival, ..)| *arrival <= at);
		self.inbound.insert(index, (at, ecn, datagram.into()));
	}

	pub(super) fn drive(&mut self, now: Instant, remote: SocketAddr) {
		self.drive_incoming(now, remote);
		self.drive_outgoing(now);
	}

	pub(super) fn drive_incoming(&mut self, now: Instant, remote: SocketAddr) {
		if let Some(socket) = &self.socket {
			loop {
				let mut buf = [0; 8192];
				if socket.recv_from(&mut buf).is_err() {
					break;
				}
			}
		}
		let buffer_size = self.endpoint.max_udp_payload_size() as usize;
		let mut buf = Vec::with_capacity(buffer_size);

		while self.inbound.front().is_some_and(|x| x.0 <= now) {
			let (recv_time, ecn, packet) = self.inbound.pop_front().unwrap();
			if let Some(event) = self.endpoint.handle(
				recv_time,
				Received {
					remote,
					local_ip: None,
					ecn,
					data: packet,
				},
				&mut buf,
			) {
				match event {
					DatagramEvent::NewConnection(incoming) => match (self.handle_incoming)(&incoming) {
						IncomingConnectionBehavior::Accept => {
							let _ = self.try_accept(incoming, now);
						}
						IncomingConnectionBehavior::Reject => {
							self.reject(incoming);
						}
						IncomingConnectionBehavior::Retry => {
							self.retry(incoming);
						}
						IncomingConnectionBehavior::Wait => {
							self.waiting_incoming.push(incoming);
						}
					},
					DatagramEvent::ConnectionEvent(ch, event) => {
						if self.capture_inbound_packets {
							let packet = self.connections[&ch].decode_packet(&event);
							self.captured_packets.extend(packet);
						}

						self.conn_events.entry(ch).or_default().push_back(event);
					}
					DatagramEvent::Response(transmit) => {
						let size = transmit.size;
						self.outbound.extend(split_transmit(transmit, &buf[..size]));
						buf.clear();
					}
				}
			}
		}
	}

	pub(super) fn drive_outgoing(&mut self, now: Instant) {
		let buffer_size = self.endpoint.max_udp_payload_size() as usize;
		let mut buf = Vec::with_capacity(buffer_size);

		loop {
			let mut endpoint_events: Vec<(ConnectionHandle, EndpointEvent)> = vec![];
			for (&ch, conn) in self.connections.iter_mut() {
				if self.timeouts.get(&ch).is_some_and(|&x| x <= now) {
					conn.handle_timeout(now);
				}

				for event in self.conn_events.remove(&ch).into_iter().flatten() {
					conn.handle_event(event);
				}

				while let Some(event) = conn.poll_endpoint_events() {
					endpoint_events.push((ch, event));
				}
				while let Some(transmit) = conn.poll_transmit(now, MAX_DATAGRAMS, &mut buf) {
					let size = transmit.size;
					self.outbound.extend(split_transmit(transmit, &buf[..size]));
					buf.clear();
				}
				match conn.poll_timeout() {
					Some(timeout) => self.timeouts.insert(ch, timeout),
					None => self.timeouts.remove(&ch),
				};
			}

			if endpoint_events.is_empty() {
				break;
			}

			for (ch, event) in endpoint_events {
				if event.is_draining() {
					self.lifecycle.push((ch, Lifecycle::Draining));
				}
				if event.is_drained() {
					self.lifecycle.push((ch, Lifecycle::Drained));
				}
				if let Some(event) = self.handle_event(ch, event)
					&& let Some(conn) = self.connections.get_mut(&ch)
				{
					conn.handle_event(event);
				}
			}
		}
	}

	pub(super) fn next_wakeup(&self) -> Option<Instant> {
		let next_timeout = self
			.timeouts
			.iter()
			.filter(|(ch, _)| self.connections.contains_key(ch))
			.map(|(_, &timeout)| timeout)
			.min();
		let next_inbound = self.inbound.front().map(|x| x.0);
		min_opt(next_timeout, next_inbound)
	}

	fn is_idle(&self) -> bool {
		self.connections.values().all(|x| x.is_idle())
	}

	pub(super) fn delay_outbound(&mut self) {
		assert!(self.delayed.is_empty());
		mem::swap(&mut self.delayed, &mut self.outbound);
	}

	pub(super) fn finish_delay(&mut self) {
		self.outbound.extend(self.delayed.drain(..));
	}

	pub(super) fn try_accept(&mut self, incoming: Incoming, now: Instant) -> Result<ConnectionHandle, ConnectionError> {
		let mut buf = Vec::new();
		match self.endpoint.accept(incoming, now, &mut buf) {
			Ok((ch, conn)) => {
				self.connections.insert(ch, conn);
				self.accepted = Some(Ok(ch));
				Ok(ch)
			}
			Err(error) => {
				if let Some(transmit) = error.response {
					let size = transmit.size;
					self.outbound.extend(split_transmit(transmit, &buf[..size]));
				}
				self.accepted = Some(Err(error.cause.clone()));
				Err(error.cause)
			}
		}
	}

	pub(super) fn pop_waiting_incoming(&mut self) -> Incoming {
		let incoming = self.waiting_incoming.pop().unwrap();
		assert!(self.waiting_incoming.is_empty());
		incoming
	}

	pub(super) fn start_split_accept(&mut self, incoming: Incoming, now: Instant) -> Accepting {
		let mut buf = Vec::new();
		self.endpoint.start_accept(incoming, now, &mut buf).unwrap()
	}

	pub(super) fn finish_split_accept(&mut self, accepting: Accepting) -> ConnectionHandle {
		let mut buf = Vec::new();
		let (ch, conn) = self
			.endpoint
			.finish_accept(accepting.accept(), &mut buf)
			.expect("split accept unexpectedly failed");
		self.connections.insert(ch, conn);
		ch
	}

	/// Like `finish_split_accept`, but expects the handshake to fail, returning the cause
	pub(super) fn finish_split_accept_error(&mut self, accepting: Accepting) -> ConnectionError {
		let mut buf = Vec::new();
		let Err(error) = self.endpoint.finish_accept(accepting.accept(), &mut buf) else {
			panic!("split accept unexpectedly succeeded")
		};
		error.cause
	}

	pub(super) fn retry(&mut self, incoming: Incoming) {
		let mut buf = Vec::new();
		let transmit = self.endpoint.retry(incoming, &mut buf).unwrap();
		let size = transmit.size;
		self.outbound.extend(split_transmit(transmit, &buf[..size]));
	}

	pub(super) fn reject(&mut self, incoming: Incoming) {
		let mut buf = Vec::new();
		let transmit = self.endpoint.refuse(incoming, &mut buf);
		let size = transmit.size;
		self.outbound.extend(split_transmit(transmit, &buf[..size]));
	}

	pub(super) fn assert_accept(&mut self) -> ConnectionHandle {
		self.accepted
			.take()
			.expect("server didn't try connecting")
			.expect("server experienced error connecting")
	}

	pub(super) fn assert_accept_error(&mut self) -> ConnectionError {
		self.accepted
			.take()
			.expect("server didn't try connecting")
			.expect_err("server did unexpectedly connect without error")
	}

	pub(super) fn assert_no_accept(&self) {
		assert!(self.accepted.is_none(), "server did unexpectedly connect")
	}
}

impl ::std::ops::Deref for TestEndpoint {
	type Target = Endpoint;
	fn deref(&self) -> &Endpoint {
		&self.endpoint
	}
}

impl ::std::ops::DerefMut for TestEndpoint {
	fn deref_mut(&mut self) -> &mut Endpoint {
		&mut self.endpoint
	}
}

pub(super) fn subscribe() -> tracing::subscriber::DefaultGuard {
	let builder = tracing_subscriber::FmtSubscriber::builder()
		.with_max_level(tracing::Level::TRACE)
		.with_writer(|| TestWriter);
	// tracing uses std::time to trace time, which panics in wasm.
	#[cfg(all(target_family = "wasm", target_os = "unknown"))]
	let builder = builder.without_time();
	tracing::subscriber::set_default(builder.finish())
}

struct TestWriter;

impl Write for TestWriter {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		print!("{}", str::from_utf8(buf).expect("tried to log invalid UTF-8"));
		Ok(buf.len())
	}
	fn flush(&mut self) -> io::Result<()> {
		io::stdout().flush()
	}
}

/// A writer whose output can be read back, e.g. to inspect a qlog trace
#[cfg(feature = "qlog")]
#[derive(Clone, Default)]
pub(super) struct SharedBuffer(pub(super) Arc<Mutex<Vec<u8>>>);

#[cfg(feature = "qlog")]
impl Write for SharedBuffer {
	fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
		self.0.lock().unwrap().extend_from_slice(buf);
		Ok(buf.len())
	}

	fn flush(&mut self) -> io::Result<()> {
		Ok(())
	}
}

pub(super) fn server_config() -> server::Config {
	let mut config = server::Config::new(Arc::new(server_crypto()));
	config.transport = transport::Config::default();

	config
}

pub(super) fn server_config_with_cert(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> server::Config {
	let mut config = server::Config::new(Arc::new(server_crypto_with_cert(cert, key)));
	config.transport = transport::Config::default();
	config.validation_token.sent = 2;
	config.validation_token.log = Arc::new(SimpleTokenLog::default());
	config
}

pub(super) fn server_crypto() -> QuicServerConfig {
	server_crypto_inner(None, None)
}

pub(super) fn server_crypto_with_alpn(alpn: Vec<Vec<u8>>) -> QuicServerConfig {
	server_crypto_inner(None, Some(alpn))
}

pub(super) fn server_crypto_with_cert(cert: CertificateDer<'static>, key: PrivateKeyDer<'static>) -> QuicServerConfig {
	server_crypto_inner(Some((cert, key)), None)
}

fn server_crypto_inner(
	identity: Option<(CertificateDer<'static>, PrivateKeyDer<'static>)>,
	alpn: Option<Vec<Vec<u8>>>,
) -> QuicServerConfig {
	let (cert, key) = identity.unwrap_or_else(|| {
		(
			CERTIFIED_KEY.cert.der().clone(),
			PrivateKeyDer::Pkcs8(CERTIFIED_KEY.signing_key.serialize_der().into()),
		)
	});

	let mut config = QuicServerConfig::inner(vec![cert], key).unwrap();
	if let Some(alpn) = alpn {
		config.alpn_protocols = alpn;
	}

	config.try_into().unwrap()
}

pub(super) fn client_config() -> client::Config {
	let mut cfg = client::Config::new(Arc::new(client_crypto()));
	cfg.transport = transport::Config::default();
	cfg
}

pub(super) fn client_config_with_deterministic_pns() -> client::Config {
	let mut cfg = client::Config::new(Arc::new(client_crypto()));
	let transport = transport::Config {
		deterministic_packet_numbers: true,
		..transport::Config::default()
	};
	cfg.transport = transport;
	cfg
}

/// Cubic instead of the production BBR3, for a test that scripts Cubic's packet schedule
///
/// At zero RTT the window-derived pacer stops pacing, so a lockstep script sees whole windows
/// leave at once, while BBR3 paces from its own rate and releases a burst nanoseconds later.
pub(super) fn cubic_transport() -> transport::Config {
	transport::Config {
		congestion: Arc::new(congestion::CubicConfig::default()),
		..transport::Config::default()
	}
}

pub(super) fn client_config_with_certs(certs: Vec<CertificateDer<'static>>) -> client::Config {
	client::Config::new(Arc::new(client_crypto_inner(Some(certs), None)))
}

pub(super) fn client_crypto() -> QuicClientConfig {
	client_crypto_inner(None, None)
}

pub(super) fn client_crypto_with_alpn(protocols: Vec<Vec<u8>>) -> QuicClientConfig {
	client_crypto_inner(None, Some(protocols))
}

fn client_crypto_inner(certs: Option<Vec<CertificateDer<'static>>>, alpn: Option<Vec<Vec<u8>>>) -> QuicClientConfig {
	let mut roots = RootCertStore::empty();
	for cert in certs.unwrap_or_else(|| vec![CERTIFIED_KEY.cert.der().clone()]) {
		roots.add(cert).unwrap();
	}

	let mut inner = QuicClientConfig::inner(
		WebPkiServerVerifier::builder_with_provider(Arc::new(roots), configured_provider())
			.build()
			.unwrap(),
	);
	inner.key_log = Arc::new(KeyLogFile::new());
	if let Some(alpn) = alpn {
		inner.alpn_protocols = alpn;
	}

	inner.try_into().unwrap()
}

pub(super) fn min_opt<T: Ord>(x: Option<T>, y: Option<T>) -> Option<T> {
	match (x, y) {
		(Some(x), Some(y)) => Some(cmp::min(x, y)),
		(Some(x), _) => Some(x),
		(_, Some(y)) => Some(y),
		_ => None,
	}
}

/// The maximum of datagrams TestEndpoint will produce via `poll_transmit`
const MAX_DATAGRAMS: usize = 10;

fn split_transmit(transmit: Transmit, buffer: &[u8]) -> Vec<(Transmit, Bytes)> {
	let mut buffer = Bytes::copy_from_slice(buffer);
	let Some(segment_size) = transmit.segment_size else {
		return vec![(transmit, buffer)];
	};

	let mut transmits = Vec::new();
	while !buffer.is_empty() {
		let end = segment_size.min(buffer.len());

		let contents = buffer.split_to(end);
		transmits.push((
			Transmit {
				remote: transmit.remote,
				size: contents.len(),
				ecn: transmit.ecn,
				segment_size: None,
				local_ip: transmit.local_ip,
			},
			contents,
		));
	}

	transmits
}

fn packet_size(transmit: &Transmit, buffer: &Bytes) -> usize {
	if transmit.segment_size.is_some() {
		panic!("This transmit is meant to be split into multiple packets!");
	}

	buffer.len()
}

fn set_congestion_experienced(x: Option<EcnCodepoint>, congestion_experienced: bool) -> Option<EcnCodepoint> {
	x.map(|codepoint| match congestion_experienced {
		true => EcnCodepoint::Ce,
		false => codepoint,
	})
}

pub(crate) static CERTIFIED_KEY: LazyLock<rcgen::CertifiedKey<rcgen::KeyPair>> =
	LazyLock::new(|| rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap());

#[derive(Default)]
struct SimpleTokenLog(Mutex<HashSet<u128>>);

impl TokenLog for SimpleTokenLog {
	fn check_and_insert(&self, nonce: u128, _issued: Wall, _lifetime: Duration) -> Result<(), TokenReuseError> {
		if self.0.lock().unwrap().insert(nonce) {
			Ok(())
		} else {
			Err(TokenReuseError)
		}
	}
}
