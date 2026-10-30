//! The `connection` target: an op sequence over a connected client and server.
//!
//! Built on the public `Endpoint` and `Connection` API rather than the test harness, so
//! it compiles on both sides of a rewrite of either. The network is a pair of FIFO
//! queues the ops drop, duplicate, reorder, corrupt, and inject into; time only moves
//! when an op says so. Both sides run [`Plaintext`] crypto, so the handshake is cheap and
//! a corrupted or injected packet reaches the frame layer instead of failing to decrypt.

use std::{
	collections::{HashMap, HashSet, VecDeque},
	net::{Ipv6Addr, SocketAddr},
	num::NonZeroU64,
	sync::Arc,
};

use arbitrary::{Arbitrary, Unstructured};
use bytes::{Bytes, BytesMut};

use super::plaintext::{Clear, Plaintext, TAG_LEN};
use crate::{
	Dir, Duration, Event, Instant, LeaseError, Limits, ReadError, Side, StreamId, client,
	connection::Connection,
	endpoint::{self, ConnectionHandle, DatagramEvent, Dial, Endpoint, Received},
	frame::{self, Frame},
	packet::{FixedLengthConnectionIdParser, PartialDecode},
	server, transport,
};

/// The connection ID length both endpoints issue, which a captured packet needs to parse.
pub(super) const CID_LEN: usize = 8;

/// The most datagrams one `poll_transmit` may batch, so GSO splitting runs too.
const MAX_DATAGRAMS: usize = 10;

/// Rounds the handshake and the final drain may take before giving up.
const ROUNDS: usize = 32;

/// The largest single write, so a long op sequence stays fast to run.
const MAX_WRITE: usize = 16 * 1024;

/// Each side's connection receive window: four of the largest writes.
const RECEIVE_WINDOW: u32 = 4 * MAX_WRITE as u32;

/// Both sides' transport: a receive window a few writes fill, so leases that wait spend it and
/// the stuck-window eviction runs.
fn transport() -> transport::Config {
	let mut transport = transport::Config::default();
	transport.limits.receive_window = NonZeroU64::new(RECEIVE_WINDOW.into());
	transport
}

/// Which endpoint an op acts on, or which one's inbound queue it touches.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum Who {
	Client,
	Server,
}

/// One step of a run. Stream indexes wrap around what that side has.
#[derive(Arbitrary, Debug)]
enum Op {
	Open {
		who: Who,
		uni: bool,
	},
	Accept {
		who: Who,
		uni: bool,
	},
	Write {
		who: Who,
		stream: u8,
		len: u16,
	},
	Finish {
		who: Who,
		stream: u8,
	},
	Reset {
		who: Who,
		stream: u8,
		code: u32,
	},
	/// Reset a stream, still delivering a prefix of what was written
	ResetAt {
		who: Who,
		stream: u8,
		offset: u16,
		code: u32,
	},
	/// Ask whether a prefix of what was written is acknowledged
	Acked {
		who: Who,
		stream: u8,
		offset: u16,
	},
	Priority {
		who: Who,
		stream: u8,
		priority: i8,
	},
	/// Rank a receive stream for eviction.
	Rank {
		who: Who,
		stream: u8,
		priority: i8,
	},
	/// Read up to `max` bytes in order, or on a leased stream lease until `max` bytes arrived and
	/// consume them, waiting otherwise.
	Read {
		who: Who,
		stream: u8,
		max: u16,
	},
	/// Consume everything every leased stream holds.
	Release {
		who: Who,
	},
	Stop {
		who: Who,
		stream: u8,
		code: u32,
	},
	MaxStreams {
		who: Who,
		_uni: bool,
		count: u8,
	},
	ReceiveWindow {
		who: Who,
		window: u32,
	},
	SendWindow {
		who: Who,
		window: u32,
	},
	Datagram {
		who: Who,
		len: u16,
		priority: i8,
	},
	Ping {
		who: Who,
	},
	KeyUpdate {
		who: Who,
	},
	/// Hand up to `count` queued datagrams to `to`.
	Deliver {
		to: Who,
		count: u8,
	},
	Drop {
		to: Who,
	},
	Duplicate {
		to: Who,
	},
	Reorder {
		to: Who,
	},
	Corrupt {
		to: Who,
		offset: u16,
		xor: u8,
	},
	/// An arbitrary datagram from the peer's address.
	Inject {
		to: Who,
		data: Vec<u8>,
	},
	/// Jump to the earliest timer either side has armed.
	Tick,
	Advance {
		millis: u16,
	},
}

/// What one side has written to a stream: its length so far and whether it finished.
#[derive(Default)]
struct Sent {
	len: u64,
	fin: bool,
}

/// A datagram in flight.
#[derive(Clone)]
struct Inbound {
	data: BytesMut,
	/// The first byte a tamper op may have changed, tainting it once the connection takes it.
	tampered: Option<usize>,
}

struct Peer {
	side: Side,
	endpoint: Endpoint,
	addr: SocketAddr,
	conn: Option<(ConnectionHandle, Connection)>,
	connected: bool,
	/// Datagrams in flight toward this side, oldest first.
	inbound: VecDeque<Inbound>,
	/// Streams this side writes: opened here, or accepted bidirectional.
	send: Vec<StreamId>,
	/// Streams this side reads: accepted, or opened here bidirectional.
	recv: Vec<StreamId>,
	/// How far each stream has been read, in order.
	read: HashMap<StreamId, u64>,
	/// The length of each datagram this side sent, indexed by the sequence number it carries.
	datagrams_sent: Vec<usize>,
	/// The sequence numbers of the datagrams this side received.
	datagrams_received: HashSet<u64>,
	/// Streams a tampered packet this side's connection took carried data for, whose reads
	/// go unchecked.
	forged_streams: HashSet<StreamId>,
	/// Payloads of the datagrams a tampered packet this side's connection took carried.
	forged_datagrams: HashSet<Bytes>,
}

impl Peer {
	fn new(side: Side, port: u16) -> Self {
		let server = side.is_server().then(|| {
			let mut config = server::Config::new(Arc::new(Plaintext));
			config.transport = transport();
			config
		});
		// A fixed seed per side keeps connection IDs and packet number skips stable across
		// runs; the reset and token keys still draw their own randomness.
		let endpoint = endpoint::Config {
			server,
			rng_seed: Some([side as u8; 32]),
			..Default::default()
		};
		Self {
			side,
			endpoint: Endpoint::new(endpoint, true).unwrap(),
			addr: SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port),
			conn: None,
			connected: false,
			inbound: VecDeque::new(),
			send: Vec::new(),
			recv: Vec::new(),
			read: HashMap::new(),
			datagrams_sent: Vec::new(),
			datagrams_received: HashSet::new(),
			forged_streams: HashSet::new(),
			forged_datagrams: HashSet::new(),
		}
	}

	fn conn(&mut self) -> Option<&mut Connection> {
		self.conn.as_mut().map(|(_, conn)| conn)
	}

	/// Record the streams and datagrams a tampered datagram this side's connection took carries.
	///
	/// Decodes it the way the endpoint and connection would, coalesced packets and all,
	/// so a frame they accept is never missed. Over-counting only skips a check.
	///
	/// `from` is the first byte that may differ from what the sender sent. Packets ending
	/// before it are untouched, and a change inside a packet's tag alters nothing: the tag
	/// is never checked and bounds no packet.
	fn taint(&mut self, datagram: &[u8], from: usize) {
		let parser = FixedLengthConnectionIdParser::new(CID_LEN);
		let mut rest = Some(BytesMut::from(datagram));
		while let Some(data) = rest.take() {
			let start = datagram.len() - data.len();
			let Ok((partial, next)) = PartialDecode::new(data, &parser, true) else {
				return;
			};
			let end = datagram.len() - next.as_ref().map_or(0, |next| next.len());
			rest = next;

			if end <= from {
				continue;
			}
			if start <= from && from + TAG_LEN >= end {
				return;
			}

			let Ok(packet) = partial.finish(Some(&Clear)) else {
				continue;
			};
			let Some(len) = packet.payload.len().checked_sub(TAG_LEN) else {
				continue;
			};
			let Ok(frames) = frame::Iter::new(packet.payload.freeze().slice(..len)) else {
				continue;
			};
			for frame in frames {
				match frame {
					Ok(Frame::Stream(stream)) => {
						self.forged_streams.insert(stream.id);
					}
					Ok(Frame::Datagram(datagram)) => {
						self.forged_datagrams.insert(datagram.data);
					}
					Ok(_) => {}
					Err(_) => break,
				}
			}
		}
	}
}

struct Pair {
	now: Instant,
	client: Peer,
	server: Peer,
	/// What each side wrote to each stream, keyed by writer, so a read can be checked.
	sent: HashMap<(Side, StreamId), Sent>,
	/// Every datagram either side sent, when collecting seeds.
	capture: Option<Vec<Vec<u8>>>,
}

impl Pair {
	/// A client and server with a connection mid-handshake.
	fn new() -> Self {
		let mut pair = Self {
			now: Instant::ZERO + Duration::from_secs(1),
			client: Peer::new(Side::Client, 4433),
			server: Peer::new(Side::Server, 4434),
			sent: HashMap::new(),
			capture: None,
		};

		let mut config = client::Config::new(Arc::new(Plaintext));
		config.transport = transport();
		let (ch, conn) = pair
			.client
			.endpoint
			.connect(
				pair.now,
				Dial {
					config: &config,
					remote: pair.server.addr,
					server_name: "localhost",
				},
			)
			.unwrap();
		pair.client.conn = Some((ch, conn));
		pair
	}

	/// Check both sides' receive lease books.
	fn check_leases(&self) {
		for peer in [&self.client, &self.server] {
			if let Some((_, conn)) = &peer.conn {
				conn.check_leases();
			}
		}
	}

	/// Drive the handshake with a lossless network, returning whether both sides connected.
	fn handshake(&mut self) -> bool {
		for _ in 0..ROUNDS {
			self.flush();
			if self.client.connected && self.server.connected {
				return true;
			}
			if self.client.inbound.is_empty() && self.server.inbound.is_empty() {
				self.tick();
			}
		}
		false
	}

	fn peer(&mut self, who: Who) -> &mut Peer {
		match who {
			Who::Client => &mut self.client,
			Who::Server => &mut self.server,
		}
	}

	/// A side and what it writes against.
	fn writer(&mut self, who: Who) -> (&mut Peer, &mut HashMap<(Side, StreamId), Sent>) {
		let Self {
			client, server, sent, ..
		} = self;
		match who {
			Who::Client => (client, sent),
			Who::Server => (server, sent),
		}
	}

	/// A side, the other side, and the capture.
	fn split(&mut self, side: Side) -> (&mut Peer, &mut Peer, &mut Option<Vec<Vec<u8>>>) {
		let Self {
			client,
			server,
			capture,
			..
		} = self;
		match side {
			Side::Client => (client, server, capture),
			Side::Server => (server, client, capture),
		}
	}

	/// Deliver every queued datagram both ways until nothing is in flight.
	fn flush(&mut self) {
		for _ in 0..ROUNDS {
			self.poll(Side::Client);
			self.poll(Side::Server);
			if self.client.inbound.is_empty() && self.server.inbound.is_empty() {
				return;
			}
			self.deliver(Side::Client, usize::MAX);
			self.deliver(Side::Server, usize::MAX);
		}
	}

	/// Hand up to `count` queued datagrams to `side`'s endpoint.
	fn deliver(&mut self, side: Side, count: usize) {
		let now = self.now;
		let (peer, other, capture) = self.split(side);
		let mut buf = Vec::new();
		for _ in 0..count {
			let Some(Inbound { data, tampered }) = peer.inbound.pop_front() else {
				break;
			};
			// Only what the connection takes can deliver frames; the endpoint drops the rest.
			let tampered = tampered.map(|from| (data.clone(), from));
			let event = peer.endpoint.handle(
				now,
				Received {
					remote: other.addr,
					local_ip: None,
					ecn: None,
					data,
				},
				&mut buf,
			);
			match event {
				Some(DatagramEvent::NewConnection(incoming)) => {
					// One connection per run: a forged Initial after it is turned away.
					if peer.conn.is_some() {
						peer.endpoint.ignore(incoming);
					} else if let Ok(accepted) = peer.endpoint.accept(incoming, now, &mut buf) {
						peer.conn = Some(accepted);
						if let Some((data, from)) = tampered {
							peer.taint(&data, from);
						}
					}
				}
				Some(DatagramEvent::ConnectionEvent(ch, event)) => {
					if peer.conn.as_ref().is_some_and(|(handle, _)| *handle == ch) {
						if let Some((data, from)) = tampered {
							peer.taint(&data, from);
						}
						peer.conn().unwrap().handle_event(event);
					}
				}
				Some(DatagramEvent::Response(transmit)) => {
					let data = &buf[..transmit.size];
					send(&mut other.inbound, capture, data, transmit.segment_size);
				}
				None => {}
			}
			buf.clear();
		}
		self.poll(side);
	}

	/// Let `side`'s connection react: fire due timers, trade endpoint events, queue
	/// whatever it transmits, and consume what it reports.
	fn poll(&mut self, side: Side) {
		let now = self.now;
		let (peer, other, capture) = self.split(side);
		let Some((ch, conn)) = &mut peer.conn else {
			return;
		};

		if conn.poll_timeout().is_some_and(|timeout| timeout <= now) {
			conn.handle_timeout(now);
		}

		let mut buf = Vec::new();
		loop {
			let mut progress = false;
			while let Some(event) = conn.poll_endpoint_events() {
				progress = true;
				if let Some(event) = peer.endpoint.handle_event(*ch, event) {
					conn.handle_event(event);
				}
			}
			while let Some(transmit) = conn.poll_transmit(now, MAX_DATAGRAMS, &mut buf) {
				progress = true;
				send(
					&mut other.inbound,
					capture,
					&buf[..transmit.size],
					transmit.segment_size,
				);
				buf.clear();
			}
			if !progress {
				break;
			}
		}

		while let Some(event) = conn.poll() {
			match event {
				Event::Connected => peer.connected = true,
				Event::DatagramReceived => {
					while let Some(datagram) = conn.datagrams().recv() {
						if peer.forged_datagrams.contains(&datagram) {
							continue;
						}
						let seq = check_datagram(&datagram, &other.datagrams_sent);
						assert!(peer.datagrams_received.insert(seq), "datagram {seq} delivered twice");
					}
				}
				_ => {}
			}
		}
	}

	/// Advance to the earliest armed timer, if any, and let both sides react.
	fn tick(&mut self) {
		let timers = [&self.client, &self.server]
			.into_iter()
			.filter_map(|peer| peer.conn.as_ref()?.1.poll_timeout());
		if let Some(next) = timers.min() {
			self.now = self.now.max(next);
		}
		self.poll(Side::Client);
		self.poll(Side::Server);
	}

	fn apply(&mut self, op: Op) {
		match op {
			Op::Open { who, uni } => {
				let peer = self.peer(who);
				let dir = dir(uni);
				let Some(id) = peer
					.conn()
					.and_then(|conn| conn.streams().open(dir, Default::default()))
				else {
					return;
				};
				peer.send.push(id);
				if dir == Dir::Bi {
					peer.recv.push(id);
				}
			}
			Op::Accept { who, uni } => {
				let peer = self.peer(who);
				let dir = dir(uni);
				let Some(id) = peer.conn().and_then(|conn| conn.streams().accept(dir)) else {
					return;
				};
				peer.recv.push(id);
				if dir == Dir::Bi {
					peer.send.push(id);
				}
			}
			Op::Write { who, stream, len } => self.write(who, stream, len),
			Op::Finish { who, stream } => {
				let (peer, sent) = self.writer(who);
				let side = peer.side;
				let Some((id, conn)) = send_stream(peer, stream) else {
					return;
				};
				if conn.send_stream(id).finish().is_ok() {
					sent.entry((side, id)).or_default().fin = true;
				}
			}
			Op::Reset { who, stream, code } => {
				if let Some((id, conn)) = send_stream(self.peer(who), stream) {
					let _ = conn.send_stream(id).reset(code.into());
				}
			}
			Op::ResetAt {
				who,
				stream,
				offset,
				code,
			} => {
				if let Some((id, conn)) = send_stream(self.peer(who), stream) {
					let mut stream = conn.send_stream(id);
					let offset = u64::from(offset) % (stream.offset() + 1);
					let _ = stream.reset_at(offset, code.into());
				}
			}
			Op::Acked { who, stream, offset } => {
				if let Some((id, conn)) = send_stream(self.peer(who), stream) {
					let mut stream = conn.send_stream(id);
					let offset = u64::from(offset) % (stream.offset() + 1);
					let _ = stream.acked(offset);
				}
			}
			Op::Priority { who, stream, priority } => {
				if let Some((id, conn)) = send_stream(self.peer(who), stream) {
					let _ = conn.send_stream(id).set_priority(priority.into());
				}
			}
			Op::Rank { who, stream, priority } => {
				let peer = self.peer(who);
				if let Some(id) = pick(&peer.recv, stream)
					&& let Some(conn) = peer.conn()
				{
					conn.recv_stream(id).set_priority(priority.into());
				}
			}
			Op::Read { who, stream, max } => {
				let (peer, sent) = self.writer(who);
				if let Some(id) = pick(&peer.recv, stream) {
					match leased(id) {
						true => lease(peer, sent, id, max.into(), max.into()),
						false => read(peer, sent, id, max.into()),
					}
				}
			}
			Op::Release { who } => {
				let (peer, sent) = self.writer(who);
				for id in peer.recv.clone() {
					if leased(id) {
						lease(peer, sent, id, 0, usize::MAX);
					}
				}
			}
			Op::Stop { who, stream, code } => {
				let peer = self.peer(who);
				let Some(id) = pick(&peer.recv, stream) else {
					return;
				};
				if let Some(conn) = peer.conn() {
					let _ = conn.recv_stream(id).stop(code.into());
				}
			}
			Op::MaxStreams { who, _uni: _, count } => {
				if let Some(conn) = self.peer(who).conn() {
					let limits = Limits {
						max_streams: NonZeroU64::new(count.into()),
						..Limits::default()
					};
					conn.set_limits(limits).unwrap();
				}
			}
			Op::ReceiveWindow { who, window } => {
				if let Some(conn) = self.peer(who).conn() {
					let limits = Limits {
						receive_window: NonZeroU64::new(window.into()),
						..Limits::default()
					};
					conn.set_limits(limits).unwrap();
				}
			}
			Op::SendWindow { who, window } => {
				if let Some(conn) = self.peer(who).conn() {
					let limits = Limits {
						send_window: NonZeroU64::new(window.into()),
						..Limits::default()
					};
					conn.set_limits(limits).unwrap();
				}
			}
			Op::Datagram { who, len, priority } => {
				let peer = self.peer(who);
				let seq = peer.datagrams_sent.len() as u64;
				let Some(conn) = peer.conn() else {
					return;
				};
				let data: Bytes = seq
					.to_le_bytes()
					.into_iter()
					.chain((0..len).map(|i| datagram_byte(seq, i.into())))
					.collect();
				let size = data.len();
				if conn
					.datagrams()
					.send(
						data,
						crate::SendOptions {
							group: None,
							priority: priority.into(),
						},
					)
					.is_ok()
				{
					peer.datagrams_sent.push(size);
				}
			}
			Op::Ping { who } => {
				if let Some(conn) = self.peer(who).conn() {
					conn.ping();
				}
			}
			Op::KeyUpdate { who } => {
				if let Some(conn) = self.peer(who).conn() {
					conn.force_key_update();
				}
			}
			Op::Deliver { to, count } => {
				let side = self.peer(to).side;
				self.deliver(side, count.into());
			}
			Op::Drop { to } => {
				self.peer(to).inbound.pop_front();
			}
			Op::Duplicate { to } => {
				let inbound = &mut self.peer(to).inbound;
				if let Some(front) = inbound.front().cloned() {
					inbound.push_front(front);
				}
			}
			Op::Reorder { to } => {
				let inbound = &mut self.peer(to).inbound;
				if inbound.len() >= 2 {
					inbound.swap(0, 1);
				}
			}
			Op::Corrupt { to, offset, xor } => {
				let peer = self.peer(to);
				// A zero XOR changes nothing, so tainting would only skip checks on real traffic.
				if xor != 0
					&& let Some(front) = peer.inbound.front_mut()
					&& !front.data.is_empty()
				{
					let offset = offset as usize % front.data.len();
					front.data[offset] ^= xor;
					front.tampered = Some(front.tampered.map_or(offset, |from| from.min(offset)));
				}
			}
			Op::Inject { to, data } => {
				self.peer(to).inbound.push_back(Inbound {
					data: BytesMut::from(&data[..]),
					tampered: Some(0),
				});
			}
			Op::Tick => self.tick(),
			Op::Advance { millis } => {
				self.now += Duration::from_millis(millis.into());
				self.poll(Side::Client);
				self.poll(Side::Server);
			}
		}

		// Writes and limit changes only queue frames; send them so the next op sees them.
		self.poll(Side::Client);
		self.poll(Side::Server);
	}

	fn write(&mut self, who: Who, stream: u8, len: u16) {
		let (peer, sent) = self.writer(who);
		let side = peer.side;
		let Some((id, conn)) = send_stream(peer, stream) else {
			return;
		};
		let sent = sent.entry((side, id)).or_default();
		let len = usize::from(len).min(MAX_WRITE) as u64;
		let data: Vec<u8> = (sent.len..sent.len + len)
			.map(|offset| stream_byte(side, id, offset))
			.collect();
		if let Ok(written) = conn.send_stream(id).write(&data) {
			sent.len += written as u64;
		}
	}

	/// Run out the network and every stream, accepting what is pending, and check all of it.
	fn drain(&mut self) {
		for _ in 0..ROUNDS {
			self.flush();
			for peer in [&mut self.client, &mut self.server] {
				for uni in [false, true] {
					while let Some(id) = peer.conn().and_then(|conn| conn.streams().accept(dir(uni))) {
						peer.recv.push(id);
					}
				}
				for id in peer.recv.clone() {
					match leased(id) {
						true => lease(peer, &self.sent, id, 0, usize::MAX),
						false => read(peer, &self.sent, id, usize::MAX),
					}
				}
			}
			self.tick();
		}
	}
}

/// Queue a transmit for the peer, split into datagrams if it was batched.
fn send(inbound: &mut VecDeque<Inbound>, capture: &mut Option<Vec<Vec<u8>>>, data: &[u8], segment_size: Option<usize>) {
	for datagram in data.chunks(segment_size.unwrap_or(data.len()).max(1)) {
		if let Some(capture) = capture {
			capture.push(datagram.to_vec());
		}
		inbound.push_back(Inbound {
			data: BytesMut::from(datagram),
			tampered: None,
		});
	}
}

fn dir(uni: bool) -> Dir {
	if uni { Dir::Uni } else { Dir::Bi }
}

fn pick(streams: &[StreamId], index: u8) -> Option<StreamId> {
	(!streams.is_empty()).then(|| streams[index as usize % streams.len()])
}

fn send_stream(peer: &mut Peer, index: u8) -> Option<(StreamId, &mut Connection)> {
	let id = pick(&peer.send, index)?;
	Some((id, peer.conn()?))
}

/// Read up to `max` bytes from a stream in order, checking each against what was sent,
/// unless a tampered packet carried data for it.
fn read(peer: &mut Peer, sent: &HashMap<(Side, StreamId), Sent>, id: StreamId, max: usize) {
	let writer = !peer.side;
	let forged = peer.forged_streams.contains(&id);
	let Some((_, conn)) = &mut peer.conn else {
		return;
	};
	let mut stream = conn.recv_stream(id);
	let Ok(mut chunks) = stream.read() else {
		return;
	};

	let offset = peer.read.entry(id).or_default();
	let mut budget = max;
	while budget > 0 {
		match chunks.next(budget) {
			Ok(Some(chunk)) => {
				let end = *offset + chunk.bytes.len() as u64;
				if !forged {
					let written = sent.get(&(writer, id)).map_or(0, |sent| sent.len);
					assert!(end <= written, "read past what was written");
					for (i, &byte) in chunk.bytes.iter().enumerate() {
						let offset = *offset + i as u64;
						assert_eq!(byte, stream_byte(writer, id, offset), "corrupt stream data");
					}
				}
				*offset = end;
				budget -= chunk.bytes.len();
			}
			Ok(None) => {
				let sent = sent.get(&(writer, id));
				if !forged {
					assert!(sent.is_some_and(|sent| sent.fin), "finished a stream never finished");
					assert_eq!(
						sent.map(|sent| sent.len),
						Some(*offset),
						"finished before all data arrived"
					);
				}
				break;
			}
			Err(ReadError::Blocked | ReadError::Reset(_)) => break,
		}
	}

	let _ = chunks.finalize();
}

/// Odd streams are read through leases, since a plain read would skip what a lease holds.
fn leased(id: StreamId) -> bool {
	id.index() % 2 == 1
}

/// Lease a stream once `want` bytes arrived, then consume up to `max` bytes of it, checking each
/// against what was sent unless a tampered packet carried data for it. An evicted stream is
/// stopped, as moq-net does.
fn lease(peer: &mut Peer, sent: &HashMap<(Side, StreamId), Sent>, id: StreamId, want: usize, max: usize) {
	let writer = !peer.side;
	let forged = peer.forged_streams.contains(&id);
	let Some((_, conn)) = &mut peer.conn else {
		return;
	};
	match conn.recv_stream(id).lease(want) {
		Ok(_) => {}
		Err(LeaseError::Evicted) => {
			conn.recv_stream(id)
				.stop(0u32.into())
				.expect("an evicted stream stops once");
			return;
		}
		// Waiting, reset, or stopped: nothing to check yet.
		Err(_) => return,
	}
	let Some(lease) = conn.leased(id) else {
		return;
	};
	let offset = peer.read.entry(id).or_default();
	let n = max.min(lease.len());
	if !forged {
		let written = sent.get(&(writer, id)).map_or(0, |sent| sent.len);
		assert!(*offset + lease.len() as u64 <= written, "leased past what was written");
		for (at, &byte) in (*offset..).zip(lease.chunks().flatten().take(n)) {
			assert_eq!(byte, stream_byte(writer, id, at), "corrupt stream data");
		}
	}
	*offset += n as u64;
	let mut recv = conn.recv_stream(id);
	recv.advance(n);
	let _ = recv.release();
}

/// The byte a writer puts at `offset` of a stream, unique enough to catch misplaced data.
fn stream_byte(writer: Side, id: StreamId, offset: u64) -> u8 {
	let mixed = offset ^ (offset >> 8) ^ (offset >> 16) ^ id.0.wrapping_mul(0x9e) ^ writer as u64;
	mixed as u8
}

/// The payload byte at index `i` of the datagram with sequence number `seq`.
fn datagram_byte(seq: u64, i: u64) -> u8 {
	(i ^ (i >> 8) ^ seq.wrapping_mul(0x3b)) as u8
}

/// Check a datagram against the sender's record and return the sequence number it carries.
fn check_datagram(datagram: &Bytes, sent: &[usize]) -> u64 {
	let (seq, payload) = datagram
		.split_first_chunk::<8>()
		.expect("datagram shorter than its sequence number");
	let seq = u64::from_le_bytes(*seq);
	let len = usize::try_from(seq)
		.ok()
		.and_then(|seq| sent.get(seq))
		.unwrap_or_else(|| panic!("datagram {seq} was never sent"));
	assert_eq!(datagram.len(), *len, "datagram {seq} changed length");
	for (i, &byte) in payload.iter().enumerate() {
		assert_eq!(byte, datagram_byte(seq, i as u64), "corrupt datagram {seq}");
	}
	seq
}

/// Handshake, apply every op the input decodes to, then drain and check.
///
/// Nothing may panic, and every byte a stream or datagram delivers must be one that was
/// written, at the offset it was written to, except on the streams and datagrams a
/// corrupted or injected packet carried.
pub fn connection(data: &[u8]) -> bool {
	let mut pair = Pair::new();
	if !pair.handshake() {
		return false;
	}

	let mut u = Unstructured::new(data);
	while !u.is_empty() {
		let Ok(op) = Op::arbitrary(&mut u) else {
			break;
		};
		pair.apply(op);
		pair.check_leases();
	}

	pair.drain();
	pair.check_leases();
	true
}

/// Every datagram a handshake and a short exchange put on the wire, for the packet seeds.
pub(super) fn datagrams() -> Vec<Vec<u8>> {
	let mut pair = Pair::new();
	pair.capture = Some(Vec::new());
	assert!(pair.handshake(), "the seed handshake did not connect");

	let ops = [
		Op::Open {
			who: Who::Client,
			uni: false,
		},
		Op::Write {
			who: Who::Client,
			stream: 0,
			len: 100,
		},
		Op::Finish {
			who: Who::Client,
			stream: 0,
		},
		Op::Datagram {
			who: Who::Server,
			len: 10,
			priority: 0,
		},
		Op::KeyUpdate { who: Who::Client },
		Op::Ping { who: Who::Client },
	];
	for op in ops {
		pair.apply(op);
	}
	pair.drain();
	pair.capture.take().unwrap()
}

#[cfg(test)]
mod tests {
	use rand::{RngExt, SeedableRng, rngs::StdRng};

	use super::*;

	/// A lossless exchange delivers everything, so the checks in `read` have something to
	/// check and the drain is long enough to finish a stream.
	#[test]
	fn exchange_completes() {
		let mut pair = Pair::new();
		assert!(pair.handshake());
		for op in [
			Op::Open {
				who: Who::Client,
				uni: true,
			},
			Op::Write {
				who: Who::Client,
				stream: 0,
				len: 5000,
			},
			Op::Finish {
				who: Who::Client,
				stream: 0,
			},
		] {
			pair.apply(op);
		}
		pair.drain();

		let id = pair.server.recv[0];
		assert_eq!(pair.server.read[&id], 5000);
	}

	/// An injected 1-RTT packet decrypts, so its frames reach the server, and the checks
	/// skip the datagram and stream it carried instead of calling them corrupt.
	#[test]
	fn injected_packet_reaches_frames() {
		let mut pair = Pair::new();
		assert!(pair.handshake());

		// Every packet toward the server after its Initial carries the CID it routes by. The
		// handshake's burst leaves the pacer empty, so give the ping time to go out.
		pair.apply(Op::Ping { who: Who::Client });
		pair.apply(Op::Advance { millis: 100 });
		let ping = pair.server.inbound.back().expect("the ping sent nothing").data.clone();
		let parser = FixedLengthConnectionIdParser::new(CID_LEN);
		let (ping, _) = PartialDecode::new(ping, &parser, true).expect("the ping did not decode");
		let dcid = ping.dst_cid();

		// A 4-byte packet number far past the client's, a DATAGRAM too short to carry a
		// sequence number, then a finished STREAM on the client's first bidi stream.
		let mut packet = vec![0x40 | 0x03];
		packet.extend_from_slice(&dcid);
		packet.extend_from_slice(&1000u32.to_be_bytes());
		packet.extend_from_slice(&[0x31, 0x02, 0xff, 0xff]);
		packet.extend_from_slice(&[0x0b, 0x00, 0x03, b'b', b'a', b'd']);
		packet.extend_from_slice(&[0; TAG_LEN]);

		pair.apply(Op::Inject {
			to: Who::Server,
			data: packet,
		});
		pair.drain();

		assert_eq!(pair.server.read.get(&StreamId(0)), Some(&3));
	}

	/// An injected packet for a CID the server never issued is dropped by the endpoint, so
	/// its frames never reach the connection and real traffic on the same stream stays checked.
	#[test]
	fn unrouted_injection_taints_nothing() {
		let mut pair = Pair::new();
		assert!(pair.handshake());

		let mut packet = vec![0x40 | 0x03];
		packet.extend_from_slice(&[0xaa; CID_LEN]);
		packet.extend_from_slice(&1000u32.to_be_bytes());
		packet.extend_from_slice(&[0x0b, 0x00, 0x03, b'b', b'a', b'd']);
		packet.extend_from_slice(&[0; TAG_LEN]);

		pair.apply(Op::Inject {
			to: Who::Server,
			data: packet,
		});
		pair.drain();

		assert!(pair.server.forged_streams.is_empty());
	}

	/// A corruption that leaves every frame intact, by flipping no bits or only a byte of
	/// the unchecked tag, leaves the datagram's streams checked.
	#[test]
	fn intact_frames_taint_nothing() {
		let mut pair = Pair::new();
		assert!(pair.handshake());
		for op in [
			Op::Open {
				who: Who::Client,
				uni: true,
			},
			Op::Write {
				who: Who::Client,
				stream: 0,
				len: 100,
			},
			Op::Advance { millis: 100 },
		] {
			pair.apply(op);
		}
		let front = pair
			.server
			.inbound
			.front()
			.expect("the write sent nothing")
			.data
			.clone();

		// The last byte belongs to the last packet's tag.
		for op in [
			Op::Corrupt {
				to: Who::Server,
				offset: 0,
				xor: 0,
			},
			Op::Corrupt {
				to: Who::Server,
				offset: (front.len() - 1).try_into().unwrap(),
				xor: 0xff,
			},
			Op::Deliver {
				to: Who::Server,
				count: 1,
			},
		] {
			pair.apply(op);
		}
		assert!(pair.server.forged_streams.is_empty());

		// The same datagram does carry stream data, so a real corruption would taint it.
		pair.server.taint(&front, 0);
		assert!(!pair.server.forged_streams.is_empty());
	}

	/// Corrupted packets decrypt and deliver mangled frames, which the checks must skip
	/// rather than report, while every untouched stream and datagram stays checked.
	#[test]
	fn corruption_never_trips_the_checks() {
		let mut rng = StdRng::seed_from_u64(0);
		let mut tainted = 0;
		for _ in 0..64 {
			let mut pair = Pair::new();
			assert!(pair.handshake());
			for _ in 0..200 {
				let who = if rng.random_bool(0.5) { Who::Client } else { Who::Server };
				let stream = rng.random();
				pair.apply(match rng.random_range(0..10) {
					0 => Op::Open {
						who,
						uni: rng.random_bool(0.5),
					},
					1 => Op::Accept {
						who,
						uni: rng.random_bool(0.5),
					},
					2 => Op::Write {
						who,
						stream,
						len: rng.random_range(0..3000),
					},
					3 => Op::Finish { who, stream },
					4 => Op::Datagram {
						who,
						len: rng.random_range(0..200),
						priority: 0,
					},
					5 => Op::Read {
						who,
						stream,
						max: rng.random(),
					},
					6 => Op::Corrupt {
						to: who,
						offset: rng.random(),
						xor: rng.random_range(1..=255),
					},
					7 => Op::Duplicate { to: who },
					8 => Op::Tick,
					_ => Op::Deliver {
						to: who,
						count: rng.random_range(0..4),
					},
				});
			}
			pair.drain();
			if !pair.client.forged_streams.is_empty() || !pair.server.forged_streams.is_empty() {
				tainted += 1;
			}
		}
		assert!(tainted > 0, "no corrupted packet carried stream data");
	}
}
