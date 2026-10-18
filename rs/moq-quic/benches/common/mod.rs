//! A sans-IO client/server harness for the benchmarks, built only from the public API so the
//! suite compiles on both sides of a rewrite.
//!
//! Packets are not protected and the congestion window is unlimited, so a benchmark measures
//! protocol CPU rather than AES or a controller's model. Time is virtual: it only advances to
//! the next timer once no datagram is in flight. Cases are timed in thread CPU time, so a busy
//! host descheduling the benchmark does not inflate them.

use std::{
    any::Any,
    collections::{HashMap, VecDeque},
    net::{Ipv6Addr, SocketAddr},
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
use moq_quic::{
    ClientConfig, ConnectError, Connection, ConnectionHandle, ConnectionId, ConnectionIdGenerator,
    DatagramEvent, Endpoint, EndpointConfig, Event, RandomConnectionIdGenerator, ReadError,
    ServerConfig, Side, StreamEvent, StreamId, TransportConfig, TransportError, VarInt,
    coding::Codec,
    congestion::{Controller, ControllerFactory},
    crypto::{
        self, CryptoError, ExportKeyingMaterialError, HeaderKey, KeyPair, Keys, PacketKey,
        UnsupportedVersion,
        rustls::{QuicClientConfig, QuicServerConfig},
    },
    transport_parameters::TransportParameters,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// The most datagrams one `poll_transmit` may batch, as a GSO-capable socket would take.
const MAX_DATAGRAMS: usize = 10;
/// The window the unlimited controller reports: far beyond anything a benchmark puts in flight.
const WINDOW: u64 = 1 << 30;
/// AEAD tag space the protocol reserves in every packet, which the no-op key leaves zeroed.
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
    /// Stop sending as soon as the stream opens.
    Stop,
}

/// What a peer's application has received so far.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Received {
    pub(crate) bytes: u64,
    pub(crate) finished: u64,
    pub(crate) reset: u64,
    pub(crate) stopped: u64,
    pub(crate) datagrams: u64,
}

/// Options for [`Pair::new`].
pub(crate) struct Setup {
    /// Client endpoints to connect, each with one connection and its own address.
    pub(crate) clients: usize,
    /// The client's connection ID length. Zero makes the client advertise an
    /// `active_connection_id_limit` of 2, otherwise 5, which bounds the CIDs the server issues.
    pub(crate) client_cid_len: usize,
}

impl Default for Setup {
    fn default() -> Self {
        Self {
            clients: 1,
            client_cid_len: 8,
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
            match self
                .endpoint
                .handle(now, remote, None, None, data, &mut buf)
            {
                Some(DatagramEvent::NewConnection(incoming)) => {
                    buf.clear();
                    match self.endpoint.accept(incoming, now, &mut buf, None) {
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
                    Event::Stream(StreamEvent::Opened { dir }) => {
                        while let Some(id) = conn.streams().accept(dir) {
                            drain(conn, id, self.sink, &mut self.received);
                        }
                    }
                    Event::Stream(StreamEvent::Readable { id }) => {
                        drain(conn, id, self.sink, &mut self.received)
                    }
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
    let mut stream = conn.recv_stream(id);
    if sink == Sink::Stop {
        if stream.stop(VarInt::from_u32(0)).is_ok() {
            received.stopped += 1;
        }
        return;
    }
    // A stream already stopped, finished, or reset is gone.
    let Ok(mut chunks) = stream.read(true) else {
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
            // Unreachable today, but keeps the suite compiling against a rewrite's new variants.
            #[allow(unreachable_patterns)]
            Err(err) => panic!("read failed: {err}"),
        }
    }
    let _ = chunks.finalize();
}

/// Splits a possibly segmented transmit into one datagram each.
fn split(out: &mut Vec<(SocketAddr, BytesMut)>, transmit: &moq_quic::Transmit, buf: &[u8]) {
    let data = &buf[..transmit.size];
    let segment = transmit.segment_size.unwrap_or(data.len());
    for datagram in data.chunks(segment) {
        out.push((transmit.destination, BytesMut::from(datagram)));
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
        let mut server_endpoint = EndpointConfig::default();
        server_endpoint
            .rng_seed(Some([0xff; 32]))
            .cid_generator(Arc::new(move || {
                Box::new(Recording {
                    inner: RandomConnectionIdGenerator::new(8),
                    log: log.clone(),
                })
            }));
        let server = Endpoint::new(Arc::new(server_endpoint), Some(server_config()), false);
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

        let client_config = client_config();
        for index in 0..setup.clients {
            let mut config = EndpointConfig::default();
            let cid_len = setup.client_cid_len;
            let mut seed = [0; 32];
            seed[..8].copy_from_slice(&(index as u64).to_le_bytes());
            config.rng_seed(Some(seed)).cid_generator(Arc::new(move || {
                Box::new(RandomConnectionIdGenerator::new(cid_len))
            }));
            let mut endpoint = Endpoint::new(Arc::new(config), None, false);
            let port = 10_000 + u16::try_from(index).expect("too many clients");
            let addr = SocketAddr::new(Ipv6Addr::LOCALHOST.into(), port);
            let (ch, conn) = endpoint
                .connect(
                    pair.now,
                    client_config.clone(),
                    pair.server.addr,
                    "localhost",
                )
                .expect("connect");
            let mut client = Peer::new(endpoint, addr);
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
    match server
        .endpoint
        .handle(pair.now, remote, None, None, datagram, &mut server.buf)
    {
        Some(DatagramEvent::ConnectionEvent(ch, event)) => {
            server.conns.get_mut(&ch).unwrap().handle_event(event)
        }
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
        assert!(
            next > pair.now,
            "datagrams stuck with {free} of {empty} bytes free"
        );
        pair.now = next;
        conn.handle_timeout(pair.now);
    }
}

/// Room for the largest burst of datagrams a benchmark queues or receives at once.
const DATAGRAM_BUFFER: usize = 64 << 20;

fn transport() -> Arc<TransportConfig> {
    let mut transport = TransportConfig::default();
    transport
        .congestion_controller_factory(Arc::new(Unlimited))
        // Pinned, so black hole detection cannot shrink packets after a benchmark's losses.
        .initial_mtu(1452)
        .min_mtu(1452)
        .max_idle_timeout(None)
        .max_concurrent_uni_streams(VarInt::from_u32(4096))
        .max_concurrent_bidi_streams(VarInt::from_u32(4096))
        .stream_receive_window(VarInt::from_u32(16 << 20))
        .receive_window(VarInt::MAX)
        .send_window(1 << 30)
        .datagram_receive_buffer_size(Some(DATAGRAM_BUFFER))
        .datagram_send_buffer_size(DATAGRAM_BUFFER);
    Arc::new(transport)
}

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    (
        key.cert.der().clone(),
        PrivateKeyDer::Pkcs8(key.signing_key.serialize_der().into()),
    )
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn server_config() -> Arc<ServerConfig> {
    let (cert, key) = IDENTITY.clone_key();
    let tls = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .unwrap();
    let crypto = QuicServerConfig::try_from(tls).unwrap();
    let mut config = ServerConfig::with_crypto(Arc::new(NoProtectionServer(Arc::new(crypto))));
    config.transport_config(transport());
    Arc::new(config)
}

fn client_config() -> ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(IDENTITY.0.clone()).unwrap();
    let tls = rustls::ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let crypto = QuicClientConfig::try_from(tls).unwrap();
    let mut config = ClientConfig::new(Arc::new(NoProtectionClient(Arc::new(crypto))));
    config.transport_config(transport());
    config
}

/// One certificate shared by every pair, since generating it dominates a small setup.
static IDENTITY: std::sync::LazyLock<Identity> = std::sync::LazyLock::new(|| {
    let (cert, key) = identity();
    Identity(cert, key)
});

struct Identity(CertificateDer<'static>, PrivateKeyDer<'static>);

impl Identity {
    fn clone_key(&self) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        (self.0.clone(), self.1.clone_key())
    }
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
struct Unlimited;

impl Controller for Unlimited {
    fn on_congestion_event(
        &mut self,
        _now: Instant,
        _sent: Instant,
        _is_persistent_congestion: bool,
        _is_ecn: bool,
        _lost_bytes: u64,
        _largest_lost: u64,
        _space: moq_quic::SpaceId,
    ) {
    }

    fn on_mtu_update(&mut self, _new_mtu: u16) {}

    fn window(&self) -> u64 {
        WINDOW
    }

    fn clone_box(&self) -> Box<dyn Controller> {
        Box::new(Self)
    }

    fn initial_window(&self) -> u64 {
        WINDOW
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

impl ControllerFactory for Unlimited {
    fn build(self: Arc<Self>, _now: Instant, _current_mtu: u16) -> Box<dyn Controller> {
        Box::new(Self)
    }
}

// No-op packet protection, ported from quinn's `perf` `noprotection.rs`: the TLS handshake runs
// for real, but every packet and header key leaves the bytes alone.

struct NoProtectionClient(Arc<QuicClientConfig>);

impl crypto::ClientConfig for NoProtectionClient {
    fn start_session(
        self: Arc<Self>,
        version: u32,
        server_name: &str,
        params: &TransportParameters,
    ) -> Result<Box<dyn crypto::Session>, ConnectError> {
        let inner = self.0.clone().start_session(version, server_name, params)?;
        Ok(Box::new(NoProtectionSession(inner)))
    }
}

struct NoProtectionServer(Arc<QuicServerConfig>);

impl crypto::ServerConfig for NoProtectionServer {
    fn initial_keys(
        &self,
        version: u32,
        dst_cid: ConnectionId,
    ) -> Result<Keys, UnsupportedVersion> {
        self.0.initial_keys(version, dst_cid).map(unprotect)
    }

    fn retry_tag(&self, version: u32, orig_dst_cid: ConnectionId, packet: &[u8]) -> [u8; 16] {
        self.0.retry_tag(version, orig_dst_cid, packet)
    }

    fn start_session(
        self: Arc<Self>,
        version: u32,
        params: &TransportParameters,
    ) -> Box<dyn crypto::Session> {
        Box::new(NoProtectionSession(
            self.0.clone().start_session(version, params),
        ))
    }
}

struct NoProtectionSession(Box<dyn crypto::Session>);

impl crypto::Session for NoProtectionSession {
    fn initial_keys(&self, dst_cid: ConnectionId, side: Side) -> Keys {
        unprotect(self.0.initial_keys(dst_cid, side))
    }

    fn handshake_data(&self) -> Option<Box<dyn Any>> {
        self.0.handshake_data()
    }

    fn peer_identity(&self) -> Option<Box<dyn Any>> {
        self.0.peer_identity()
    }

    fn early_crypto(&self) -> Option<(Box<dyn HeaderKey>, Box<dyn PacketKey>)> {
        let (header, packet) = self.0.early_crypto()?;
        Some((
            Box::new(NoHeader(header.sample_size())),
            Box::new(NoPacket(packet.tag_len())),
        ))
    }

    fn early_data_accepted(&self) -> Option<bool> {
        self.0.early_data_accepted()
    }

    fn is_handshaking(&self) -> bool {
        self.0.is_handshaking()
    }

    fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError> {
        self.0.read_handshake(buf)
    }

    fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError> {
        self.0.transport_parameters()
    }

    fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys> {
        self.0.write_handshake(buf).map(unprotect)
    }

    fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn PacketKey>>> {
        let keys = self.0.next_1rtt_keys()?;
        Some(KeyPair {
            local: Box::new(NoPacket(keys.local.tag_len())),
            remote: Box::new(NoPacket(keys.remote.tag_len())),
        })
    }

    fn is_valid_retry(&self, orig_dst_cid: ConnectionId, header: &[u8], payload: &[u8]) -> bool {
        self.0.is_valid_retry(orig_dst_cid, header, payload)
    }

    fn export_keying_material(
        &self,
        output: &mut [u8],
        label: &[u8],
        context: &[u8],
    ) -> Result<(), ExportKeyingMaterialError> {
        self.0.export_keying_material(output, label, context)
    }
}

fn unprotect(keys: Keys) -> Keys {
    Keys {
        header: KeyPair {
            local: Box::new(NoHeader(keys.header.local.sample_size())),
            remote: Box::new(NoHeader(keys.header.remote.sample_size())),
        },
        packet: KeyPair {
            local: Box::new(NoPacket(keys.packet.local.tag_len())),
            remote: Box::new(NoPacket(keys.packet.remote.tag_len())),
        },
    }
}

/// Header protection that leaves the header in the clear.
struct NoHeader(usize);

impl HeaderKey for NoHeader {
    fn decrypt(&self, _pn_offset: usize, _packet: &mut [u8]) {}

    fn encrypt(&self, _pn_offset: usize, _packet: &mut [u8]) {}

    fn sample_size(&self) -> usize {
        self.0
    }
}

/// Packet protection that zeroes the tag on the way out and strips it on the way in.
///
/// Its limits never trigger a key update, which a forged packet could not follow.
struct NoPacket(usize);

impl PacketKey for NoPacket {
    fn encrypt(&self, _packet: u64, buf: &mut [u8], _header_len: usize) {
        let tag = buf.len() - self.0;
        buf[tag..].fill(0);
    }

    fn decrypt(
        &self,
        _packet: u64,
        _header: &[u8],
        payload: &mut BytesMut,
    ) -> Result<(), CryptoError> {
        let len = payload.len().checked_sub(self.0).ok_or(CryptoError)?;
        payload.truncate(len);
        Ok(())
    }

    fn tag_len(&self) -> usize {
        self.0
    }

    fn confidentiality_limit(&self) -> u64 {
        u64::MAX
    }

    fn integrity_limit(&self) -> u64 {
        u64::MAX
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
