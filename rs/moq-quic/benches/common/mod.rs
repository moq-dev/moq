//! A sans-IO client/server harness for the benchmarks, built from the public API so the suite
//! compiles on both sides of a rewrite.
//!
//! Both sides run the hidden `fuzz` module's plaintext crypto, which skips TLS and leaves packets
//! unprotected, and the congestion window is unlimited, so a benchmark measures protocol CPU
//! rather than AES or a controller's model. Time is virtual: it only advances to
//! the next timer once no datagram is in flight. Cases are timed in thread CPU time, so a busy
//! host descheduling the benchmark does not inflate them.

use std::{
	any::Any,
	collections::{HashMap, VecDeque},
	net::{Ipv6Addr, SocketAddr},
	num::NonZeroU64,
	sync::{Arc, Mutex},
	time::Duration,
};

use bytes::{BufMut, Bytes, BytesMut};
use cpu_time::ThreadTime;
use criterion::{
	Criterion,
	measurement::{Measurement, ValueFormatter, WallTime},
};
use moq_clock::Instant;
use moq_congestion::{Ack, Ce, Controller, ControllerFactory, ControllerMetrics, Init, Loss, Packet, Sent};
use moq_quic::{
	ConnectionId, ConnectionIdGenerator, Event, LeaseError, RandomConnectionIdGenerator, ReadError, StreamEvent,
	StreamId, VarInt, client,
	connection::Connection,
	endpoint::{self, ConnectionHandle, DatagramEvent, Dial, Endpoint, Received as Inbound},
	fuzz::Plaintext,
	server, transport,
};

/// The most datagrams one `poll_transmit` may batch, as a GSO-capable socket would take.
const MAX_DATAGRAMS: usize = 10;
/// The window the unlimited controller reports: far beyond anything a benchmark puts in flight.
const WINDOW: u64 = 1 << 30;
/// The send window unless a benchmark caps it: far beyond anything a benchmark writes.
const SEND_WINDOW: u64 = 1 << 30;
/// AEAD tag space the protocol reserves in every packet, which plaintext crypto leaves zeroed.
const TAG_LEN: usize = 16;
/// How far virtual time may jump to a timer while settling before the pair counts as idle.
const SETTLE: Duration = Duration::from_secs(1);
/// Virtual time between forged iterations: well past two PTOs on a zero-latency path, after which
/// a connection forgets the packets it declared lost.
const TICK: Duration = Duration::from_millis(100);
/// How far virtual time may jump to a timer before a stuck exchange fails loudly.
const STALL: Duration = Duration::from_secs(10);

/// How a peer's application handles each stream the other side opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sink {
	/// Read everything to the end or the reset.
	Read,
	/// Lease until at least this many bytes have arrived, then release everything leased. A
	/// stream evicted while it waits is stopped, as moq-net does.
	Lease(usize),
	/// Stop sending as soon as the stream opens.
	Stop,
	/// Leave every stream unaccepted, as an application that has yet to run.
	Ignore,
}

/// What a peer's application has received so far.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Received {
	pub(crate) bytes: u64,
	pub(crate) finished: u64,
	pub(crate) reset: u64,
	pub(crate) stopped: u64,
	pub(crate) evicted: u64,
	pub(crate) datagrams: u64,
}

/// Options for [`Pair::new`].
pub(crate) struct Setup {
	/// Client endpoints to connect, each with one connection and its own address.
	pub(crate) clients: usize,
	/// The client's connection ID length. Zero makes the client advertise an
	/// `active_connection_id_limit` of 2, otherwise 5, which bounds the CIDs the server issues.
	pub(crate) client_cid_len: usize,
	/// The server's connection receive window, unbounded by default.
	pub(crate) server_receive_window: VarInt,
	/// The client's send window: how many written bytes it retains until they are acknowledged.
	pub(crate) client_send_window: u64,
	/// The rate the client probes up to, in bytes per second, under a controller that always
	/// lets it; zero never probes.
	pub(crate) client_probe_target: u64,
}

impl Default for Setup {
	fn default() -> Self {
		Self {
			clients: 1,
			client_cid_len: 8,
			server_receive_window: VarInt::MAX,
			client_send_window: SEND_WINDOW,
			client_probe_target: 0,
		}
	}
}

/// One endpoint and its connections, with an application that drains whatever arrives.
pub(crate) struct Peer {
	pub(crate) endpoint: Endpoint,
	pub(crate) addr: SocketAddr,
	pub(crate) conns: HashMap<ConnectionHandle, Connection>,
	pub(crate) sink: Sink,
	pub(crate) received: Received,
	/// Wall time spent in [`Peer::process`], to time one side of an exchange.
	pub(crate) busy: Duration,
	inbound: VecDeque<(SocketAddr, BytesMut)>,
	outbound: Vec<(SocketAddr, BytesMut)>,
	buf: Vec<u8>,
}

impl Peer {
	fn new(endpoint: Endpoint, addr: SocketAddr) -> Self {
		Self {
			endpoint,
			addr,
			conns: HashMap::new(),
			sink: Sink::Read,
			received: Received::default(),
			busy: Duration::ZERO,
			inbound: VecDeque::new(),
			outbound: Vec::new(),
			buf: Vec::new(),
		}
	}

	/// The only connection, for a peer that has exactly one.
	pub(crate) fn conn(&mut self) -> &mut Connection {
		assert_eq!(self.conns.len(), 1, "peer has more than one connection");
		self.conns.values_mut().next().unwrap()
	}

	/// Handles every queued datagram, fires due timers, runs the application, and transmits.
	fn process(&mut self, now: Instant) {
		let start = ThreadTime::now();
		let mut buf = std::mem::take(&mut self.buf);

		for (remote, data) in std::mem::take(&mut self.inbound) {
			buf.clear();
			match self.endpoint.handle(
				now,
				Inbound {
					remote,
					local_ip: None,
					ecn: None,
					data,
				},
				&mut buf,
			) {
				Some(DatagramEvent::NewConnection(incoming)) => {
					buf.clear();
					match self.endpoint.accept(incoming, now, &mut buf) {
						Ok((ch, conn)) => {
							self.conns.insert(ch, conn);
						}
						Err(err) => panic!("accept failed: {:?}", err.cause),
					}
				}
				Some(DatagramEvent::ConnectionEvent(ch, event)) => {
					self.conns
						.get_mut(&ch)
						.expect("routed to a connection")
						.handle_event(event);
				}
				Some(DatagramEvent::Response(transmit)) => {
					split(&mut self.outbound, &transmit, &buf);
				}
				None => {}
			}
		}

		for (&ch, conn) in self.conns.iter_mut() {
			if conn.poll_timeout().is_some_and(|at| at <= now) {
				conn.handle_timeout(now);
			}
			while let Some(event) = conn.poll_endpoint_events() {
				if let Some(event) = self.endpoint.handle_event(ch, event) {
					conn.handle_event(event);
				}
			}
			while let Some(event) = conn.poll() {
				match event {
					Event::ConnectionLost { reason } => panic!("connection lost: {reason}"),
					Event::Stream(StreamEvent::Opened { .. }) if self.sink == Sink::Ignore => {}
					Event::Stream(StreamEvent::Opened { dir }) => {
						while let Some(id) = conn.streams().accept(dir) {
							drain(conn, id, self.sink, &mut self.received);
						}
					}
					Event::Stream(StreamEvent::Readable { id }) => drain(conn, id, self.sink, &mut self.received),
					// The application answers STOP_SENDING the way a writer does: it gives up.
					Event::Stream(StreamEvent::Stopped { id, error_code }) => {
						let _ = conn.send_stream(id).reset(error_code);
					}
					Event::DatagramReceived => {
						while conn.datagrams().recv().is_some() {
							self.received.datagrams += 1;
						}
					}
					_ => {}
				}
			}
			buf.clear();
			while let Some(transmit) = conn.poll_transmit(now, MAX_DATAGRAMS, &mut buf) {
				split(&mut self.outbound, &transmit, &buf);
				buf.clear();
			}
		}

		self.buf = buf;
		self.busy += start.elapsed();
	}

	fn next_timeout(&self) -> Option<Instant> {
		self.conns.values().filter_map(|c| c.poll_timeout()).min()
	}
}

/// Reads or stops one stream, recording what the application saw.
fn drain(conn: &mut Connection, id: StreamId, sink: Sink, received: &mut Received) {
	if let Sink::Lease(want) = sink {
		return lease(conn, id, want, received);
	}
	if sink == Sink::Ignore {
		return;
	}
	let mut stream = conn.recv_stream(id);
	if sink == Sink::Stop {
		if stream.stop(VarInt::from_u32(0)).is_ok() {
			received.stopped += 1;
		}
		return;
	}
	// A stream already stopped, finished, or reset is gone.
	let Ok(mut chunks) = stream.read() else {
		return;
	};
	loop {
		match chunks.next(usize::MAX) {
			Ok(Some(chunk)) => received.bytes += chunk.bytes.len() as u64,
			Ok(None) => {
				received.finished += 1;
				break;
			}
			Err(ReadError::Blocked) => break,
			Err(ReadError::Reset(_)) => {
				received.reset += 1;
				break;
			}
			#[allow(
				unreachable_patterns,
				reason = "unreachable today, but keeps the suite compiling against a rewrite's new variants"
			)]
			Err(err) => panic!("read failed: {err}"),
		}
	}
	let _ = chunks.finalize();
}

/// Leases one stream until it waits, releasing whatever it leased.
///
/// A stream that ends is stopped, as a driver does when its reader drops, which is what
/// discards its lease.
fn lease(conn: &mut Connection, id: StreamId, want: usize, received: &mut Received) {
	loop {
		let ended = match conn.recv_stream(id).lease(want) {
			Ok(_) => None,
			Err(LeaseError::Blocked(_) | LeaseError::ClosedStream) => return,
			Err(LeaseError::Reset(_)) => Some(&mut received.reset),
			Err(LeaseError::Evicted) => Some(&mut received.evicted),
			Err(err) => panic!("lease failed: {err}"),
		};
		if let Some(count) = ended {
			*count += 1;
			let _ = conn.recv_stream(id).stop(VarInt::from_u32(0));
			return;
		}
		let fin = conn.leased(id).expect("leased above").is_fin();
		let mut stream = conn.recv_stream(id);
		// Taken a chunk at a time, as moq-net hands a frame's payload on.
		loop {
			let chunk = stream.take(usize::MAX);
			if chunk.is_empty() {
				break;
			}
			received.bytes += chunk.len() as u64;
		}
		let _ = stream.release();
		if fin {
			received.finished += 1;
			let _ = stream.stop(VarInt::from_u32(0));
			return;
		}
	}
}

/// Splits a possibly segmented transmit into one datagram each.
fn split(out: &mut Vec<(SocketAddr, BytesMut)>, transmit: &moq_quic::Transmit, buf: &[u8]) {
	let data = &buf[..transmit.size];
	let segment = transmit.segment_size.unwrap_or(data.len());
	for datagram in data.chunks(segment) {
		out.push((transmit.remote, BytesMut::from(datagram)));
	}
}

/// A server and its connected clients, exchanging datagrams over a zero-latency path.
pub(crate) struct Pair {
	pub(crate) now: Instant,
	pub(crate) server: Peer,
	pub(crate) clients: Vec<Peer>,
	/// Every connection ID the server's endpoint generated, in order.
	pub(crate) server_cids: Arc<Mutex<Vec<ConnectionId>>>,
	/// Drops every Nth client datagram, so the server holds data past each hole until the
	/// retransmission fills it. Reordering instead would have to stay within the packet number
	/// window the sender encoded for, a few dozen packets.
	pub(crate) lose_every: Option<u64>,
	sent: u64,
	by_addr: HashMap<SocketAddr, usize>,
}

impl Pair {
	/// Connects `setup.clients` clients to one server and settles every handshake.
	pub(crate) fn new(setup: Setup) -> Self {
		let server_cids = Arc::new(Mutex::new(Vec::new()));
		let log = server_cids.clone();
		// Fixed seeds make packet number skips and key update timing the same on every run, and
		// on both sides of a comparison.
		let mut server_endpoint = endpoint::Config::default();
		server_endpoint.rng_seed = Some([0xff; 32]);
		server_endpoint.cid_generator = Some(Arc::new(move || {
			Box::new(Recording {
				inner: RandomConnectionIdGenerator::new(8),
				log: log.clone(),
			})
		}));
		server_endpoint.server = Some(server_config(setup.server_receive_window));
		let server = Endpoint::new(server_endpoint, false).unwrap();
		let server = Peer::new(server, SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 4433));

		let mut pair = Self {
			now: Instant::ZERO + Duration::from_secs(3600),
			server,
			clients: Vec::new(),
			server_cids,
			lose_every: None,
			sent: 0,
			by_addr: HashMap::new(),
		};

		let client_config = client_config(setup.client_send_window, setup.client_probe_target > 0);
		for index in 0..setup.clients {
			let mut config = endpoint::Config::default();
			let cid_len = setup.client_cid_len;
			let mut seed = [0; 32];
			seed[..8].copy_from_slice(&(index as u64).to_le_bytes());
			config.rng_seed = Some(seed);
			config.cid_generator = Some(Arc::new(move || Box::new(RandomConnectionIdGenerator::new(cid_len))));
			let mut endpoint = Endpoint::new(config, false).unwrap();
			let port = 10_000 + u16::try_from(index).expect("too many clients");
			let addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
			let (ch, conn) = endpoint
				.connect(
					pair.now,
					Dial {
						config: &client_config,
						remote: pair.server.addr,
						server_name: "localhost",
					},
				)
				.expect("connect");
			let mut client = Peer::new(endpoint, addr);
			let mut conn = conn;
			conn.set_probe_target(setup.client_probe_target);
			client.conns.insert(ch, conn);
			pair.by_addr.insert(addr, index);
			pair.clients.push(client);
		}

		pair.drive_until(|pair| {
			pair.server.conns.len() == setup.clients
				&& pair
					.server
					.conns
					.values()
					.chain(pair.clients.iter().flat_map(|c| c.conns.values()))
					.all(|c| !c.is_handshaking())
		});
		pair.settle();
		pair
	}

	/// The only client's connection.
	pub(crate) fn client(&mut self) -> &mut Connection {
		assert_eq!(self.clients.len(), 1, "pair has more than one client");
		self.clients[0].conn()
	}

	/// Runs every peer once and delivers what they sent, returning whether anything moved.
	pub(crate) fn step(&mut self) -> bool {
		let mut moved = false;
		for client in &mut self.clients {
			client.process(self.now);
			for (destination, datagram) in client.outbound.drain(..) {
				assert_eq!(destination, self.server.addr);
				moved = true;
				self.sent += 1;
				if self.lose_every.is_some_and(|n| self.sent.is_multiple_of(n)) {
					continue;
				}
				self.server.inbound.push_back((client.addr, datagram));
			}
		}

		self.server.process(self.now);
		for (destination, datagram) in self.server.outbound.drain(..) {
			let client = &mut self.clients[self.by_addr[&destination]];
			client.inbound.push_back((self.server.addr, datagram));
			moved = true;
		}
		moved
	}

	/// Exchanges datagrams, jumping to the next timer whenever the path is idle, until `done`.
	pub(crate) fn drive_until(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
		loop {
			let moved = self.step();
			if done(self) {
				return;
			}
			if !moved {
				let next = self.next_timeout();
				let received = self.server.received;
				let next = next.unwrap_or_else(|| panic!("stalled with no timer: {received:?}"));
				assert!(
					next <= self.now + STALL,
					"stalled with no timer due within {STALL:?}: {received:?}"
				);
				self.now = self.now.max(next);
			}
		}
	}

	/// Exchanges datagrams until nothing is in flight and no timer is due soon.
	pub(crate) fn settle(&mut self) {
		loop {
			if self.step() {
				continue;
			}
			match self.next_timeout() {
				Some(next) if next <= self.now + SETTLE => self.now = self.now.max(next),
				_ => return,
			}
		}
	}

	fn next_timeout(&self) -> Option<Instant> {
		self.clients
			.iter()
			.chain(std::iter::once(&self.server))
			.filter_map(Peer::next_timeout)
			.min()
	}
}

/// Builds client 1-RTT packets by hand, for a server connection whose real client has gone quiet.
///
/// With no packet or header protection, a short header packet is its plaintext: the first byte,
/// the server's connection ID, a 4-byte packet number, the frames, and zeroed tag space.
pub(crate) struct Forge {
	dcid: ConnectionId,
	next: u64,
}

impl Forge {
	/// Forges for the pair's only server connection, numbering well past the real client's.
	pub(crate) fn new(pair: &Pair) -> Self {
		Self {
			dcid: pair.server_cids.lock().unwrap()[0],
			next: 1 << 20,
		}
	}

	/// Wraps `frames` in the next packet.
	pub(crate) fn packet(&mut self, frames: &[u8]) -> BytesMut {
		let mut packet = BytesMut::with_capacity(1 + self.dcid.len() + 4 + frames.len() + TAG_LEN);
		packet.put_u8(0x40 | 0x03);
		packet.put_slice(&self.dcid);
		packet.put_u32(self.next as u32);
		packet.put_slice(frames);
		packet.put_bytes(0, TAG_LEN);
		self.next += 1;
		packet
	}
}

/// Appends a DATAGRAM frame carrying `data`.
pub(crate) fn datagram_frame(out: &mut BytesMut, data: &[u8]) {
	VarInt::from_u32(0x31).encode(out);
	VarInt::from_u64(data.len() as u64).unwrap().encode(out);
	out.put_slice(data);
}

/// Appends an ACK frame for `ranges`, inclusive and sorted from the highest packet down.
pub(crate) fn ack_frame(out: &mut BytesMut, ranges: &[(u64, u64)]) {
	let varint = |out: &mut BytesMut, x: u64| VarInt::from_u64(x).unwrap().encode(out);
	let (first_start, largest) = ranges[0];
	varint(out, 0x02);
	varint(out, largest);
	varint(out, 0); // ACK delay
	varint(out, ranges.len() as u64 - 1);
	varint(out, largest - first_start);
	let mut previous = first_start;
	for &(start, end) in &ranges[1..] {
		varint(out, previous - end - 2);
		varint(out, end - start);
		previous = start;
	}
}

/// Recovers the full packet numbers of server 1-RTT datagrams sent to a client, in order.
pub(crate) struct ServerPackets {
	client_cid_len: usize,
	largest: Option<u64>,
}

impl ServerPackets {
	pub(crate) fn new(client_cid_len: usize) -> Self {
		Self {
			client_cid_len,
			largest: None,
		}
	}

	/// The packet number of one short header datagram, per RFC 9000 appendix A.3.
	pub(crate) fn number(&mut self, datagram: &[u8]) -> u64 {
		assert_eq!(datagram[0] & 0x80, 0, "expected a short header packet");
		let len = usize::from(datagram[0] & 0x03) + 1;
		let start = 1 + self.client_cid_len;
		let truncated = datagram[start..start + len]
			.iter()
			.fold(0u64, |pn, &byte| (pn << 8) | u64::from(byte));

		let expected = self.largest.map_or(0, |pn| pn + 1);
		let window = 1u64 << (len * 8);
		let candidate = (expected & !(window - 1)) | truncated;
		let pn = if candidate + window / 2 <= expected {
			candidate + window
		} else if candidate > expected + window / 2 && candidate >= window {
			candidate - window
		} else {
			candidate
		};
		assert!(
			self.largest.is_none_or(|largest| pn > largest),
			"packet numbers went backwards"
		);
		self.largest = Some(pn);
		pn
	}
}

/// Delivers one forged datagram to the pair's only server connection.
pub(crate) fn deliver(pair: &mut Pair, datagram: BytesMut) {
	let remote = pair.clients[0].addr;
	let server = &mut pair.server;
	server.buf.clear();
	match server.endpoint.handle(
		pair.now,
		Inbound {
			remote,
			local_ip: None,
			ecn: None,
			data: datagram,
		},
		&mut server.buf,
	) {
		Some(DatagramEvent::ConnectionEvent(ch, event)) => server.conns.get_mut(&ch).unwrap().handle_event(event),
		_ => panic!("forged datagram was not routed to the connection"),
	}
}

/// Lets virtual time pass on the pair's only server connection and fires its due timers.
///
/// A forged exchange never waits on a timer, so without this, state that expires with time, such
/// as packets remembered as lost, would pile up across iterations.
pub(crate) fn tick(pair: &mut Pair) {
	pair.now += TICK;
	let conn = pair.server.conn();
	if conn.poll_timeout().is_some_and(|at| at <= pair.now) {
		conn.handle_timeout(pair.now);
	}
}

/// Transmits from the pair's only server connection until its datagram send buffer is back to
/// `empty` free bytes, jumping virtual time over pacing delays, and returns the datagrams.
pub(crate) fn flush(pair: &mut Pair, empty: usize) -> Vec<BytesMut> {
	let mut out = Vec::new();
	let server = &mut pair.server;
	let conn = server.conns.values_mut().next().unwrap();
	loop {
		server.buf.clear();
		while let Some(transmit) = conn.poll_transmit(pair.now, MAX_DATAGRAMS, &mut server.buf) {
			split(&mut out, &transmit, &server.buf);
			server.buf.clear();
		}
		let free = conn.datagrams().send_buffer_space();
		if free == empty {
			return out.into_iter().map(|(_, datagram)| datagram).collect();
		}
		// Pacing is the only reason to stop short.
		let next = conn.poll_timeout().expect("paced with no timer");
		assert!(next > pair.now, "datagrams stuck with {free} of {empty} bytes free");
		pair.now = next;
		conn.handle_timeout(pair.now);
	}
}

/// Room for the largest burst of datagrams a benchmark queues or receives at once.
const DATAGRAM_BUFFER: usize = 64 << 20;

fn transport(receive_window: VarInt, send_window: u64, probe: bool) -> transport::Config {
	let mut transport = transport::Config::default();
	transport.congestion = Arc::new(Unlimited { probe });
	// Pinned, so black hole detection cannot shrink packets after a benchmark's losses.
	transport.initial_mtu = 1452;
	transport.min_mtu = 1452;
	transport.idle_timeout = None;
	transport.limits.max_streams = NonZeroU64::new(4096);
	transport.limits.stream_receive_window = NonZeroU64::new(16 << 20);
	transport.limits.receive_window = NonZeroU64::new(receive_window.into_inner());
	transport.limits.send_window = NonZeroU64::new(send_window);
	transport.datagram.receive_buffer = Some(DATAGRAM_BUFFER);
	transport.datagram.send_buffer = DATAGRAM_BUFFER;
	transport
}

fn server_config(receive_window: VarInt) -> server::Config {
	let mut config = server::Config::new(Arc::new(Plaintext));
	config.transport = transport(receive_window, SEND_WINDOW, false);
	config
}

fn client_config(send_window: u64, probe: bool) -> client::Config {
	let mut config = client::Config::new(Arc::new(Plaintext));
	config.transport = transport(VarInt::MAX, send_window, probe);
	config
}

/// A server connection ID generator that records every ID, so benchmarks can address them.
struct Recording {
	inner: RandomConnectionIdGenerator,
	log: Arc<Mutex<Vec<ConnectionId>>>,
}

impl ConnectionIdGenerator for Recording {
	fn generate_cid(&mut self) -> ConnectionId {
		let cid = self.inner.generate_cid();
		self.log.lock().unwrap().push(cid);
		cid
	}

	fn cid_len(&self) -> usize {
		self.inner.cid_len()
	}

	fn cid_lifetime(&self) -> Option<Duration> {
		self.inner.cid_lifetime()
	}
}

/// A congestion controller that never limits, so benchmarks measure the protocol, not a model.
///
/// One that probes lets its sender fill every gap with duplicates, up to its probe target.
#[derive(Clone, Copy)]
struct Unlimited {
	probe: bool,
}

impl Controller for Unlimited {
	fn on_sent(&mut self, _sent: Sent) -> Packet {
		Packet::default()
	}

	fn on_ack(&mut self, _ack: Ack<'_>) {}

	fn on_loss(&mut self, _loss: Loss<'_>) {}

	fn on_ce(&mut self, _ce: Ce) {}

	fn on_mtu_update(&mut self, _new_mtu: u16) {}

	fn window(&self) -> u64 {
		WINDOW
	}

	fn pace(&mut self, _now: Instant, _bytes: u64) -> Option<Instant> {
		None
	}

	fn metrics(&self) -> ControllerMetrics {
		ControllerMetrics {
			congestion_window: WINDOW,
			probe_inflight: self.probe.then_some(WINDOW),
			..Default::default()
		}
	}

	fn probes(&self) -> bool {
		self.probe
	}

	fn clone_box(&self) -> Box<dyn Controller> {
		Box::new(*self)
	}

	fn into_any(self: Box<Self>) -> Box<dyn Any> {
		self
	}
}

impl ControllerFactory for Unlimited {
	fn build(self: Arc<Self>, _init: Init) -> Box<dyn Controller> {
		Box::new(*self)
	}
}

/// The Criterion config every target runs with: thread CPU time, not elapsed time.
///
/// The suite is single-threaded, so this thread's CPU time is the protocol's, and a loaded host
/// preempting the benchmark does not count toward it.
pub(crate) fn config() -> Criterion<ThreadCpu> {
	Criterion::default().with_measurement(ThreadCpu)
}

/// Measures this thread's CPU time.
pub(crate) struct ThreadCpu;

impl Measurement for ThreadCpu {
	type Intermediate = ThreadTime;
	type Value = Duration;

	fn start(&self) -> ThreadTime {
		ThreadTime::now()
	}

	fn end(&self, start: ThreadTime) -> Duration {
		start.elapsed()
	}

	fn add(&self, a: &Duration, b: &Duration) -> Duration {
		*a + *b
	}

	fn zero(&self) -> Duration {
		Duration::ZERO
	}

	fn to_f64(&self, value: &Duration) -> f64 {
		value.as_nanos() as f64
	}

	fn formatter(&self) -> &dyn ValueFormatter {
		// Durations, so they format the way elapsed time does.
		static WALL: WallTime = WallTime;
		WALL.formatter()
	}
}

/// A buffer of `len` bytes, for payloads whose content does not matter.
pub(crate) fn payload(len: usize) -> Bytes {
	Bytes::from(vec![0xab; len])
}
