//! The `connection` target: an op sequence over a connected client and server.
//!
//! Built on the public `Endpoint` and `Connection` API rather than the test harness, so
//! it compiles on both sides of a rewrite of either. The network is a pair of FIFO
//! queues the ops drop, duplicate, reorder, corrupt, and inject into; time only moves
//! when an op says so.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    net::{Ipv6Addr, SocketAddr},
    sync::{Arc, LazyLock},
};

use arbitrary::{Arbitrary, Unstructured};
use bytes::{Bytes, BytesMut};
use rustls::{RootCertStore, client::WebPkiServerVerifier, pki_types::PrivateKeyDer};

use crate::{
    ClientConfig, Connection, ConnectionHandle, Credit, DatagramEvent, Dir, Duration, Endpoint,
    EndpointConfig, Event, Instant, ReadError, ServerConfig, Side, StreamId,
    crypto::rustls::{QuicClientConfig, QuicServerConfig, configured_provider},
};

/// The connection ID length both endpoints issue, which a captured packet needs to parse.
pub(super) const CID_LEN: usize = 8;

/// The most datagrams one `poll_transmit` may batch, so GSO splitting runs too.
const MAX_DATAGRAMS: usize = 10;

/// Rounds the handshake and the final drain may take before giving up.
const ROUNDS: usize = 32;

/// The largest single write, so a long op sequence stays fast to run.
const MAX_WRITE: usize = 16 * 1024;

struct Configs {
    server: Arc<ServerConfig>,
    client: ClientConfig,
}

/// Built once per process: generating a certificate costs more than a handshake.
static CONFIGS: LazyLock<Configs> = LazyLock::new(|| {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert = certified.cert.der().clone();
    let key = PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());

    let server = QuicServerConfig::inner(vec![cert.clone()], key).unwrap();
    let server = ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(server).unwrap()));

    let mut roots = RootCertStore::empty();
    roots.add(cert).unwrap();
    let verifier =
        WebPkiServerVerifier::builder_with_provider(Arc::new(roots), configured_provider())
            .build()
            .unwrap();
    let client = QuicClientConfig::try_from(QuicClientConfig::inner(verifier)).unwrap();
    let client = ClientConfig::new(Arc::new(client));

    Configs {
        server: Arc::new(server),
        client,
    }
});

/// Which endpoint an op acts on, or which one's inbound queue it touches.
#[derive(Arbitrary, Debug, Clone, Copy)]
enum Who {
    Client,
    Server,
}

/// One step of a run. Stream and checkpoint indexes wrap around what that side has.
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
    Checkpoint {
        who: Who,
        stream: u8,
    },
    ResetCheckpoint {
        who: Who,
        checkpoint: u8,
        code: u32,
    },
    Priority {
        who: Who,
        stream: u8,
        priority: i8,
    },
    Read {
        who: Who,
        stream: u8,
        max: u16,
        hold: bool,
    },
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
        uni: bool,
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
        order: i8,
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

struct Peer {
    side: Side,
    endpoint: Endpoint,
    addr: SocketAddr,
    conn: Option<(ConnectionHandle, Connection)>,
    connected: bool,
    /// Datagrams in flight toward this side, oldest first.
    inbound: VecDeque<BytesMut>,
    /// Streams this side writes: opened here, or accepted bidirectional.
    send: Vec<StreamId>,
    /// Streams this side reads: accepted, or opened here bidirectional.
    recv: Vec<StreamId>,
    /// How far each stream has been read, in order.
    read: HashMap<StreamId, u64>,
    checkpoints: Vec<crate::Checkpoint>,
    held: Vec<Credit>,
    /// The length of each datagram this side sent, indexed by the sequence number it carries.
    datagrams_sent: Vec<usize>,
    /// The sequence numbers of the datagrams this side received.
    datagrams_received: HashSet<u64>,
}

impl Peer {
    fn new(side: Side, port: u16) -> Self {
        let server = side.is_server().then(|| CONFIGS.server.clone());
        // A fixed seed per side keeps connection IDs and tokens stable across runs; TLS
        // still draws its own randomness.
        let mut endpoint = EndpointConfig::default();
        endpoint.rng_seed(Some([side as u8; 32]));
        Self {
            side,
            endpoint: Endpoint::new(Arc::new(endpoint), server, true),
            addr: SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port),
            conn: None,
            connected: false,
            inbound: VecDeque::new(),
            send: Vec::new(),
            recv: Vec::new(),
            read: HashMap::new(),
            checkpoints: Vec::new(),
            held: Vec::new(),
            datagrams_sent: Vec::new(),
            datagrams_received: HashSet::new(),
        }
    }

    fn conn(&mut self) -> Option<&mut Connection> {
        self.conn.as_mut().map(|(_, conn)| conn)
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

        let (ch, conn) = pair
            .client
            .endpoint
            .connect(
                pair.now,
                CONFIGS.client.clone(),
                pair.server.addr,
                "localhost",
            )
            .unwrap();
        pair.client.conn = Some((ch, conn));
        pair
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
            client,
            server,
            sent,
            ..
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
            let Some(datagram) = peer.inbound.pop_front() else {
                break;
            };
            let event = peer
                .endpoint
                .handle(now, other.addr, None, None, datagram, &mut buf);
            match event {
                Some(DatagramEvent::NewConnection(incoming)) => {
                    // One connection per run: a forged Initial after it is turned away.
                    if peer.conn.is_some() {
                        peer.endpoint.ignore(incoming);
                    } else if let Ok(accepted) = peer.endpoint.accept(incoming, now, &mut buf, None)
                    {
                        peer.conn = Some(accepted);
                    }
                }
                Some(DatagramEvent::ConnectionEvent(ch, event)) => {
                    if let Some((handle, conn)) = &mut peer.conn
                        && *handle == ch
                    {
                        conn.handle_event(event);
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
                        let seq = check_datagram(&datagram, &other.datagrams_sent);
                        assert!(
                            peer.datagrams_received.insert(seq),
                            "datagram {seq} delivered twice"
                        );
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
                let Some(id) = peer.conn().and_then(|conn| conn.streams().open(dir)) else {
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
            Op::Checkpoint { who, stream } => {
                let peer = self.peer(who);
                if let Some((id, conn)) = send_stream(peer, stream) {
                    let checkpoint = conn.send_stream(id).checkpoint();
                    peer.checkpoints.push(checkpoint);
                }
            }
            Op::ResetCheckpoint {
                who,
                checkpoint,
                code,
            } => {
                let peer = self.peer(who);
                if peer.checkpoints.is_empty() {
                    return;
                }
                let index = checkpoint as usize % peer.checkpoints.len();
                let checkpoint = peer.checkpoints.swap_remove(index);
                if let Some(conn) = peer.conn() {
                    let _ = checkpoint.reset(conn, code.into());
                }
            }
            Op::Priority {
                who,
                stream,
                priority,
            } => {
                if let Some((id, conn)) = send_stream(self.peer(who), stream) {
                    let _ = conn.send_stream(id).set_priority(priority.into());
                }
            }
            Op::Read {
                who,
                stream,
                max,
                hold,
            } => {
                let (peer, sent) = self.writer(who);
                if let Some(id) = pick(&peer.recv, stream) {
                    read(peer, sent, id, max.into(), hold);
                }
            }
            Op::Release { who } => {
                let peer = self.peer(who);
                let held = std::mem::take(&mut peer.held);
                if let Some(conn) = peer.conn() {
                    for credit in held {
                        let _ = conn.release(credit);
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
            Op::MaxStreams { who, uni, count } => {
                let dir = dir(uni);
                if let Some(conn) = self.peer(who).conn() {
                    conn.set_max_concurrent_streams(dir, count.into());
                }
            }
            Op::ReceiveWindow { who, window } => {
                if let Some(conn) = self.peer(who).conn() {
                    conn.set_receive_window(window.into());
                }
            }
            Op::SendWindow { who, window } => {
                if let Some(conn) = self.peer(who).conn() {
                    conn.set_send_window(window.into());
                }
            }
            Op::Datagram { who, len, order } => {
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
                if conn.datagrams().send(data, None, order.into()).is_ok() {
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
                if let Some(front) = self.peer(to).inbound.front_mut()
                    && !front.is_empty()
                {
                    let offset = offset as usize % front.len();
                    front[offset] ^= xor;
                }
            }
            Op::Inject { to, data } => {
                self.peer(to).inbound.push_back(BytesMut::from(&data[..]));
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
                    while let Some(id) =
                        peer.conn().and_then(|conn| conn.streams().accept(dir(uni)))
                    {
                        peer.recv.push(id);
                    }
                }
                for id in peer.recv.clone() {
                    read(peer, &self.sent, id, usize::MAX, false);
                }
            }
            self.tick();
        }
    }
}

/// Queue a transmit for the peer, split into datagrams if it was batched.
fn send(
    inbound: &mut VecDeque<BytesMut>,
    capture: &mut Option<Vec<Vec<u8>>>,
    data: &[u8],
    segment_size: Option<usize>,
) {
    for datagram in data.chunks(segment_size.unwrap_or(data.len()).max(1)) {
        if let Some(capture) = capture {
            capture.push(datagram.to_vec());
        }
        inbound.push_back(BytesMut::from(datagram));
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

/// Read up to `max` bytes from a stream in order, checking each against what was sent.
fn read(
    peer: &mut Peer,
    sent: &HashMap<(Side, StreamId), Sent>,
    id: StreamId,
    max: usize,
    hold: bool,
) {
    let writer = !peer.side;
    let Some((_, conn)) = &mut peer.conn else {
        return;
    };
    let mut stream = conn.recv_stream(id);
    let Ok(mut chunks) = stream.read(true) else {
        return;
    };

    let offset = peer.read.entry(id).or_default();
    let mut budget = max;
    while budget > 0 {
        match chunks.next(budget) {
            Ok(Some(chunk)) => {
                assert_eq!(
                    chunk.offset, *offset,
                    "ordered read skipped or repeated data"
                );
                let end = chunk.offset + chunk.bytes.len() as u64;
                let written = sent.get(&(writer, id)).map_or(0, |sent| sent.len);
                assert!(end <= written, "read past what was written");
                for (i, &byte) in chunk.bytes.iter().enumerate() {
                    let offset = chunk.offset + i as u64;
                    assert_eq!(byte, stream_byte(writer, id, offset), "corrupt stream data");
                }
                *offset = end;
                budget -= chunk.bytes.len();
            }
            Ok(None) => {
                let sent = sent.get(&(writer, id));
                assert!(
                    sent.is_some_and(|sent| sent.fin),
                    "finished a stream never finished"
                );
                assert_eq!(
                    sent.map(|sent| sent.len),
                    Some(*offset),
                    "finished before all data arrived"
                );
                break;
            }
            Err(ReadError::Blocked | ReadError::Reset(_)) => break,
        }
    }

    if hold {
        peer.held.push(chunks.hold());
    }
    let _ = chunks.finalize();
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
/// written, at the offset it was written to.
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
    }

    pair.drain();
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
            order: 0,
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
}
