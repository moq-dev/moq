use alloc::borrow::ToOwned;
use alloc::vec;
use alloc::vec::Vec;
use std::println;

use super::*;
use crate::association::Event;
use crate::config::generate_snap_token;
use crate::error::{Error, Result};

use crate::association::state::{AckMode, AssociationState};
use crate::association::stream::{ReliabilityType, Stream, StreamEvent, StreamResetError};
use crate::chunk::chunk_abort::ChunkAbort;
use crate::chunk::chunk_cookie_echo::ChunkCookieEcho;
use crate::chunk::chunk_error::ChunkError;
use crate::chunk::chunk_forward_tsn::ChunkForwardTsn;
use crate::chunk::chunk_heartbeat::ChunkHeartbeat;
use crate::chunk::chunk_init::ChunkInit;
use crate::chunk::chunk_payload_data::{ChunkPayloadData, PayloadProtocolIdentifier};
use crate::chunk::chunk_reconfig::ChunkReconfig;
use crate::chunk::chunk_selective_ack::{ChunkSelectiveAck, GapAckBlock};
use crate::chunk::chunk_shutdown::ChunkShutdown;
use crate::chunk::chunk_shutdown_ack::ChunkShutdownAck;
use crate::chunk::chunk_shutdown_complete::ChunkShutdownComplete;
use crate::chunk::{ErrorCauseProtocolViolation, PROTOCOL_VIOLATION};
use crate::packet::{CommonHeader, Packet};
use crate::param::param_outgoing_reset_request::ParamOutgoingResetRequest;
use crate::param::param_reconfig_response::ParamReconfigResponse;
use crate::param::param_supported_address_types::ParamSupportedAddressTypes;
use crate::param::param_type::ParamType;
use assert_matches::assert_matches;
use core::net::Ipv6Addr;
use core::ops::RangeFrom;
use core::str::FromStr;
use core::time::Duration;
use core::{cmp, mem};
use log::{info, trace};
use std::net::UdpSocket;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

pub static SERVER_PORTS: LazyLock<Mutex<RangeFrom<u16>>> = LazyLock::new(|| Mutex::new(4433..));
pub static CLIENT_PORTS: LazyLock<Mutex<RangeFrom<u16>>> = LazyLock::new(|| Mutex::new(44433..));

fn min_opt<T: Ord>(x: Option<T>, y: Option<T>) -> Option<T> {
    match (x, y) {
        (Some(x), Some(y)) => Some(cmp::min(x, y)),
        (Some(x), _) => Some(x),
        (_, Some(y)) => Some(y),
        _ => None,
    }
}

/// The maximum of datagrams TestEndpoint will produce via `poll_transmit`
const MAX_DATAGRAMS: usize = 10;

fn split_transmit(transmit: Transmit) -> Vec<Transmit> {
    let mut transmits = Vec::new();
    if let Payload::RawEncode(contents) = transmit.payload {
        for content in contents {
            transmits.push(Transmit {
                now: transmit.now,
                remote: transmit.remote,
                payload: Payload::RawEncode(vec![content]),
                ecn: transmit.ecn,
                local_ip: transmit.local_ip,
            });
        }
    }

    transmits
}

pub fn client_config() -> ClientConfig {
    ClientConfig::new()
}

pub fn server_config() -> ServerConfig {
    ServerConfig::new()
}

struct TestEndpoint {
    endpoint: Endpoint,
    addr: SocketAddr,
    socket: Option<UdpSocket>,
    timeout: Option<Instant>,
    outbound: VecDeque<Transmit>,
    delayed: VecDeque<Transmit>,
    inbound: VecDeque<(Instant, Option<EcnCodepoint>, Bytes)>,
    accepted: Option<AssociationHandle>,
    associations: HashMap<AssociationHandle, Association>,
    conn_events: HashMap<AssociationHandle, VecDeque<AssociationEvent>>,
}

impl TestEndpoint {
    fn new(endpoint: Endpoint, addr: SocketAddr) -> Self {
        let socket = UdpSocket::bind(addr).expect("failed to bind UDP socket");
        socket
            .set_read_timeout(Some(Duration::new(0, 10_000_000)))
            .unwrap();

        Self {
            endpoint,
            addr,
            socket: Some(socket),
            timeout: None,
            outbound: VecDeque::new(),
            delayed: VecDeque::new(),
            inbound: VecDeque::new(),
            accepted: None,
            associations: HashMap::default(),
            conn_events: HashMap::default(),
        }
    }

    pub fn drive(&mut self, now: Instant, remote: SocketAddr) {
        if let Some(ref socket) = self.socket {
            loop {
                let mut buf = [0; 8192];
                if socket.recv_from(&mut buf).is_err() {
                    break;
                }
            }
        }

        while self.inbound.front().is_some_and(|x| x.0 <= now) {
            let (recv_time, ecn, packet) = self.inbound.pop_front().unwrap();
            if let Some((ch, event)) = self.endpoint.handle(recv_time, remote, None, ecn, packet) {
                match event {
                    DatagramEvent::NewAssociation(conn) => {
                        self.associations.insert(ch, conn);
                        self.accepted = Some(ch);
                    }
                    DatagramEvent::AssociationEvent(event) => {
                        self.conn_events.entry(ch).or_default().push_back(event);
                    }
                }
            }
        }

        while let Some(x) = self.poll_transmit() {
            self.outbound.extend(split_transmit(x));
        }

        let mut endpoint_events: Vec<(AssociationHandle, EndpointEvent)> = vec![];
        for (ch, conn) in self.associations.iter_mut() {
            if self.timeout.is_some_and(|x| x <= now) {
                self.timeout = None;
                conn.handle_timeout(now);
            }

            for (_, mut events) in self.conn_events.drain() {
                for event in events.drain(..) {
                    conn.handle_event(event);
                }
            }

            while let Some(event) = conn.poll_endpoint_event() {
                endpoint_events.push((*ch, event));
            }

            while let Some(x) = conn.poll_transmit(now) {
                self.outbound.extend(split_transmit(x));
            }
            self.timeout = conn.poll_timeout();
        }

        for (ch, event) in endpoint_events {
            if let Some(event) = self.handle_event(ch, event) {
                if let Some(conn) = self.associations.get_mut(&ch) {
                    conn.handle_event(event);
                }
            }
        }
    }

    pub fn next_wakeup(&self) -> Option<Instant> {
        let next_inbound = self.inbound.front().map(|x| x.0);
        min_opt(self.timeout, next_inbound)
    }

    fn is_idle(&self) -> bool {
        self.associations.values().all(|x| x.is_idle())
    }

    pub fn delay_outbound(&mut self) {
        assert!(self.delayed.is_empty());
        mem::swap(&mut self.delayed, &mut self.outbound);
    }

    pub fn finish_delay(&mut self) {
        self.outbound.extend(self.delayed.drain(..));
    }

    pub fn assert_accept(&mut self) -> AssociationHandle {
        self.accepted.take().expect("server didn't connect")
    }
}

impl ::core::ops::Deref for TestEndpoint {
    type Target = Endpoint;
    fn deref(&self) -> &Endpoint {
        &self.endpoint
    }
}

impl ::core::ops::DerefMut for TestEndpoint {
    fn deref_mut(&mut self) -> &mut Endpoint {
        &mut self.endpoint
    }
}

struct Pair {
    server: TestEndpoint,
    client: TestEndpoint,
    time: Instant,
    latency: Duration, // One-way
}

impl Pair {
    pub fn new(endpoint_config: Arc<EndpointConfig>, server_config: ServerConfig) -> Self {
        let server = Endpoint::new(endpoint_config.clone(), Some(Arc::new(server_config)));
        let client = Endpoint::new(endpoint_config, None);

        Pair::new_from_endpoint(client, server)
    }

    pub fn new_from_endpoint(client: Endpoint, server: Endpoint) -> Self {
        let server_addr = SocketAddr::new(
            Ipv6Addr::LOCALHOST.into(),
            SERVER_PORTS.lock().unwrap().next().unwrap(),
        );
        let client_addr = SocketAddr::new(
            Ipv6Addr::LOCALHOST.into(),
            CLIENT_PORTS.lock().unwrap().next().unwrap(),
        );
        Self {
            server: TestEndpoint::new(server, server_addr),
            client: TestEndpoint::new(client, client_addr),
            time: Instant::now(),
            latency: Duration::new(0, 0),
        }
    }

    /// Returns whether the association is not idle
    pub fn step(&mut self) -> bool {
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
                    trace!("advancing to {:?} for client", self.time);
                }
                true
            }
            Some(t) if Some(t) == server_t => {
                if t != self.time {
                    self.time = self.time.max(t);
                    trace!("advancing to {:?} for server", self.time);
                }
                true
            }
            Some(_) => unreachable!(),
            None => false,
        }
    }

    /// Advance time until both associations are idle
    pub fn drive(&mut self) {
        while self.step() {}
    }

    pub fn drive_client(&mut self) {
        self.client.drive(self.time, self.server.addr);
        for x in self.client.outbound.drain(..) {
            if let Payload::RawEncode(contents) = x.payload {
                for content in contents {
                    if let Some(ref socket) = self.client.socket {
                        socket.send_to(&content, x.remote).unwrap();
                    }
                    if self.server.addr == x.remote {
                        self.server
                            .inbound
                            .push_back((self.time + self.latency, x.ecn, content));
                    }
                }
            }
        }
    }

    pub fn drive_server(&mut self) {
        self.server.drive(self.time, self.client.addr);
        for x in self.server.outbound.drain(..) {
            if let Payload::RawEncode(contents) = x.payload {
                for content in contents {
                    if let Some(ref socket) = self.server.socket {
                        socket.send_to(&content, x.remote).unwrap();
                    }
                    if self.client.addr == x.remote {
                        self.client
                            .inbound
                            .push_back((self.time + self.latency, x.ecn, content));
                    }
                }
            }
        }
    }

    pub fn connect(&mut self) -> (AssociationHandle, AssociationHandle) {
        self.connect_with(client_config())
    }

    pub fn connect_with(&mut self, config: ClientConfig) -> (AssociationHandle, AssociationHandle) {
        info!("connecting");
        let client_ch = self.begin_connect(config);
        self.drive();
        let server_ch = self.server.assert_accept();
        self.finish_connect(client_ch, server_ch);
        (client_ch, server_ch)
    }

    /// Just start connecting the client
    pub fn begin_connect(&mut self, config: ClientConfig) -> AssociationHandle {
        let (client_ch, client_conn) = self.client.connect(config, self.server.addr).unwrap();
        self.client.associations.insert(client_ch, client_conn);
        client_ch
    }

    fn finish_connect(&mut self, client_ch: AssociationHandle, server_ch: AssociationHandle) {
        assert_matches!(
            self.client_conn_mut(client_ch).poll(),
            Some(Event::Connected)
        );

        assert_matches!(
            self.server_conn_mut(server_ch).poll(),
            Some(Event::Connected)
        );
    }

    pub fn client_conn_mut(&mut self, ch: AssociationHandle) -> &mut Association {
        self.client.associations.get_mut(&ch).unwrap()
    }

    pub fn client_stream(&mut self, ch: AssociationHandle, si: u16) -> Result<Stream<'_>> {
        self.client_conn_mut(ch).stream(si)
    }

    pub fn server_conn_mut(&mut self, ch: AssociationHandle) -> &mut Association {
        self.server.associations.get_mut(&ch).unwrap()
    }

    pub fn server_stream(&mut self, ch: AssociationHandle, si: u16) -> Result<Stream<'_>> {
        self.server_conn_mut(ch).stream(si)
    }
}

impl Default for Pair {
    fn default() -> Self {
        Pair::new(Default::default(), server_config())
    }
}

fn create_association_pair(
    ack_mode: AckMode,
    recv_buf_size: u32,
) -> Result<(Pair, AssociationHandle, AssociationHandle)> {
    let mut pair = Pair::new(
        Arc::new(EndpointConfig::default()),
        ServerConfig {
            transport: Arc::new(if recv_buf_size > 0 {
                TransportConfig::default().with_max_receive_buffer_size(recv_buf_size)
            } else {
                TransportConfig::default()
            }),
            ..Default::default()
        },
    );
    let (client_ch, server_ch) = pair.connect_with(ClientConfig {
        transport: Arc::new(if recv_buf_size > 0 {
            TransportConfig::default().with_max_receive_buffer_size(recv_buf_size)
        } else {
            TransportConfig::default()
        }),
        ..Default::default()
    });
    pair.client_conn_mut(client_ch).ack_mode = ack_mode;
    pair.server_conn_mut(server_ch).ack_mode = ack_mode;
    Ok((pair, client_ch, server_ch))
}

fn establish_session_pair(
    pair: &mut Pair,
    client_ch: AssociationHandle,
    server_ch: AssociationHandle,
    si: u16,
) -> Result<()> {
    let hello_msg = Bytes::from_static(b"Hello");
    let _ = pair
        .client_conn_mut(client_ch)
        .open_stream(si, PayloadProtocolIdentifier::Binary)?;
    let _ = pair
        .client_stream(client_ch, si)?
        .write_sctp(&hello_msg, PayloadProtocolIdentifier::Dcep)?;
    pair.drive();

    {
        let s1 = pair.server_conn_mut(server_ch).accept_stream().unwrap();
        if si != s1.stream_identifier {
            return Err(Error::Other("si should match".to_owned()));
        }
    }
    pair.drive();

    let mut buf = vec![0u8; 1024];
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let n = chunks.read(&mut buf)?;

    if n != hello_msg.len() {
        return Err(Error::Other("received data must by 3 bytes".to_owned()));
    }

    if chunks.ppi != PayloadProtocolIdentifier::Dcep {
        return Err(Error::Other("unexpected ppi".to_owned()));
    }

    if buf[..n] != hello_msg {
        return Err(Error::Other("received data mismatch".to_owned()));
    }
    pair.drive();

    Ok(())
}

fn close_association_pair(
    _pair: &mut Pair,
    _client_ch: AssociationHandle,
    _server_ch: AssociationHandle,
    _si: u16,
) {
    /*TODO:
    // Close client
    tokio::spawn(async move {
        client.close().await?;
        let _ = handshake0ch_tx.send(()).await;
        let _ = closed_rx0.recv().await;

        Result::<()>::Ok(())
    });

    // Close server
    tokio::spawn(async move {
        server.close().await?;
        let _ = handshake1ch_tx.send(()).await;
        let _ = closed_rx1.recv().await;

        Result::<()>::Ok(())
    });
    */
}

#[test]
fn test_assoc_reliable_simple() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;
    let msg: Bytes = Bytes::from_static(b"ABC");

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(0, a.buffered_amount(), "incorrect bufferedAmount");
    }

    let n = pair
        .client_stream(client_ch, si)?
        .write_sctp(&msg, PayloadProtocolIdentifier::Binary)?;
    assert_eq!(msg.len(), n, "unexpected length of received data");
    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(msg.len(), a.buffered_amount(), "incorrect bufferedAmount");
    }

    pair.drive();

    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    assert_eq!(n, msg.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(0, a.buffered_amount(), "incorrect bufferedAmount");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reliable_ordered_reordered() -> Result<()> {
    // let _guard = subscribe();

    let si: u16 = 2;
    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(0, a.buffered_amount(), "incorrect bufferedAmount");
    }

    sbuf[0..4].copy_from_slice(&0u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.delay_outbound(); // Delay it

    sbuf[0..4].copy_from_slice(&1u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.finish_delay(); // Reorder it

    pair.drive();

    let mut buf = vec![0u8; 2000];

    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        0,
        "unexpected received data"
    );

    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        1,
        "unexpected received data"
    );

    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reliable_ordered_fragmented_then_defragmented() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 3;
    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }
    let mut sbufl = vec![0u8; 2000];
    for (i, b) in sbufl.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    pair.client_stream(client_ch, si)?.set_reliability_params(
        false,
        ReliabilityType::Reliable,
        0,
    )?;
    pair.server_stream(server_ch, si)?.set_reliability_params(
        false,
        ReliabilityType::Reliable,
        0,
    )?;

    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbufl.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbufl.len(), n, "unexpected length of received data");

    pair.drive();

    let mut rbuf = vec![0u8; 2000];
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut rbuf)?;
    assert_eq!(n, sbufl.len(), "unexpected length of received data");
    assert_eq!(&rbuf[..n], &sbufl, "unexpected received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reliable_unordered_fragmented_then_defragmented() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 4;
    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }
    let sbufl = vec![0u8; 2000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    pair.client_stream(client_ch, si)?.set_reliability_params(
        true,
        ReliabilityType::Reliable,
        0,
    )?;
    pair.server_stream(server_ch, si)?.set_reliability_params(
        true,
        ReliabilityType::Reliable,
        0,
    )?;

    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbufl.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbufl.len(), n, "unexpected length of received data");

    pair.drive();

    let mut rbuf = vec![0u8; 2000];
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut rbuf)?;
    assert_eq!(n, sbufl.len(), "unexpected length of received data");
    assert_eq!(&rbuf[..n], &sbufl, "unexpected received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reliable_unordered_ordered() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 5;
    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    pair.client_stream(client_ch, si)?.set_reliability_params(
        true,
        ReliabilityType::Reliable,
        0,
    )?;
    pair.server_stream(server_ch, si)?.set_reliability_params(
        true,
        ReliabilityType::Reliable,
        0,
    )?;

    sbuf[0..4].copy_from_slice(&0u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.delay_outbound(); // Delay it

    sbuf[0..4].copy_from_slice(&1u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.finish_delay(); // Reorder it

    pair.drive();

    let mut buf = vec![0u8; 2000];

    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        1,
        "unexpected received data"
    );

    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        0,
        "unexpected received data"
    );

    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reliable_retransmission() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 6;
    let msg1: Bytes = Bytes::from_static(b"ABC");
    let msg2: Bytes = Bytes::from_static(b"DEFG");

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    {
        let a = pair.client_conn_mut(client_ch);
        a.rto_mgr.set_rto(100, true);
    }

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    let n = pair
        .client_stream(client_ch, si)?
        .write_sctp(&msg1, PayloadProtocolIdentifier::Binary)?;
    assert_eq!(msg1.len(), n, "unexpected length of received data");
    pair.drive_client(); // send data to server
    pair.server.inbound.clear(); // Lose it
    debug!("dropping packet");

    let n = pair
        .client_stream(client_ch, si)?
        .write_sctp(&msg2, PayloadProtocolIdentifier::Binary)?;
    assert_eq!(msg2.len(), n, "unexpected length of received data");

    pair.drive();

    let mut buf = vec![0u8; 32];

    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, msg1.len(), "unexpected length of received data");
    assert_eq!(&buf[..n], &msg1, "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, msg2.len(), "unexpected length of received data");
    assert_eq!(&buf[..n], &msg2, "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    let sent = pair.client_stream(client_ch, si)?.stats()?;
    let received = pair.server_stream(server_ch, si)?.stats()?;
    assert_eq!((sent.messages_sent, sent.bytes_sent), (2, 7));
    assert_eq!(
        (received.messages_received, received.bytes_received),
        (2, 7)
    );

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reliable_short_buffer() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;
    let msg: Bytes = Bytes::from_static(b"Hello");

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(0, a.buffered_amount(), "incorrect bufferedAmount");
    }

    let n = pair
        .client_stream(client_ch, si)?
        .write_sctp(&msg, PayloadProtocolIdentifier::Binary)?;
    assert_eq!(msg.len(), n, "unexpected length of received data");
    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(msg.len(), a.buffered_amount(), "incorrect bufferedAmount");
    }

    pair.drive();

    let mut buf = vec![0u8; 3];
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let result = chunks.read(&mut buf);
    assert!(result.is_err(), "expected error to be io.ErrShortBuffer");
    if let Err(err) = result {
        assert_eq!(
            Error::ErrShortBuffer,
            err,
            "expected error to be io.ErrShortBuffer"
        );
    }

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(0, a.buffered_amount(), "incorrect bufferedAmount");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_unreliable_rexmit_ordered_no_fragment() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;
    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // When we set the reliability value to 0 [times], then it will cause
    // the chunk to be abandoned immediately after the first transmission.
    pair.client_stream(client_ch, si)?
        .set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
    pair.server_stream(server_ch, si)?
        .set_reliability_params(false, ReliabilityType::Rexmit, 0)?; // doesn't matter

    //br.drop_next_nwrites(0, 1).await; // drop the first packet (second one should be sacked)

    sbuf[0..4].copy_from_slice(&0u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.drive_client(); // send data to server
    pair.server.inbound.clear(); // Lose it
    debug!("dropping packet");

    sbuf[0..4].copy_from_slice(&1u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");

    debug!("flush_buffers");
    pair.drive();

    let mut buf = vec![0u8; 2000];

    debug!("read_sctp");
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        1,
        "unexpected received data"
    );

    debug!("process");
    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_unreliable_rexmit_ordered_fragment() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;
    let mut sbuf = vec![0u8; 2000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        // lock RTO value at 100 [msec]
        let a = pair.client_conn_mut(client_ch);
        a.rto_mgr.set_rto(100, true);
    }
    // When we set the reliability value to 0 [times], then it will cause
    // the chunk to be abandoned immediately after the first transmission.
    pair.client_stream(client_ch, si)?
        .set_reliability_params(false, ReliabilityType::Rexmit, 0)?;
    pair.server_stream(server_ch, si)?
        .set_reliability_params(false, ReliabilityType::Rexmit, 0)?; // doesn't matter

    //br.drop_next_nwrites(0, 1).await; // drop the first packet (second one should be sacked)

    sbuf[0..4].copy_from_slice(&0u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.drive_client(); // send data to server
    pair.server.inbound.clear(); // Lose it

    sbuf[0..4].copy_from_slice(&1u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");

    //log::debug!("flush_buffers");
    pair.drive();

    let mut buf = vec![0u8; 2000];

    //log::debug!("read_sctp");
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        1,
        "unexpected received data"
    );

    //log::debug!("process");
    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_unreliable_rexmit_unordered_no_fragment() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 2;
    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // When we set the reliability value to 0 [times], then it will cause
    // the chunk to be abandoned immediately after the first transmission.
    pair.client_stream(client_ch, si)?
        .set_reliability_params(true, ReliabilityType::Rexmit, 0)?;
    pair.server_stream(server_ch, si)?
        .set_reliability_params(true, ReliabilityType::Rexmit, 0)?; // doesn't matter

    //br.drop_next_nwrites(0, 1).await; // drop the first packet (second one should be sacked)

    sbuf[0..4].copy_from_slice(&0u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.drive_client(); // send data to server
    pair.server.inbound.clear(); // Lose it

    sbuf[0..4].copy_from_slice(&1u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");

    //log::debug!("flush_buffers");
    pair.drive();

    let mut buf = vec![0u8; 2000];

    //log::debug!("read_sctp");
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        1,
        "unexpected received data"
    );

    //log::debug!("process");
    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_unreliable_rexmit_unordered_fragment() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;
    let mut sbuf = vec![0u8; 2000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // When we set the reliability value to 0 [times], then it will cause
    // the chunk to be abandoned immediately after the first transmission.
    pair.client_stream(client_ch, si)?
        .set_reliability_params(true, ReliabilityType::Rexmit, 0)?;
    pair.server_stream(server_ch, si)?
        .set_reliability_params(true, ReliabilityType::Rexmit, 0)?; // doesn't matter

    sbuf[0..4].copy_from_slice(&0u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.client.drive(pair.time, pair.server.addr);
    pair.client.outbound.clear();
    //debug!("outbound len={}", pair.client.outbound.len());

    sbuf[0..4].copy_from_slice(&1u32.to_be_bytes());
    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");

    pair.drive();

    let mut buf = vec![0u8; 2000];

    //log::debug!("read_sctp");
    let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
    let (n, ppi) = (chunks.len(), chunks.ppi);
    chunks.read(&mut buf)?;
    assert_eq!(n, sbuf.len(), "unexpected length of received data");
    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
    assert_eq!(
        u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
        1,
        "unexpected received data"
    );

    //log::debug!("process");
    pair.drive();

    {
        let q = &pair
            .client_conn_mut(client_ch)
            .streams
            .get(&si)
            .unwrap()
            .reassembly_queue;
        assert!(!q.is_readable(), "should no longer be readable");
        assert_eq!(
            0,
            q.unordered.len(),
            "should be nothing in the unordered queue"
        );
        assert_eq!(
            0,
            q.unordered_chunks.len(),
            "should be nothing in the unorderedChunks list"
        );
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_pending_timed_expiry_window_stall() -> Result<()> {
    for unordered in [false, true] {
        for initial_ssn in [1, u16::MAX] {
            let (mut pair, client, server) = create_association_pair(AckMode::NoDelay, 0)?;
            establish_session_pair(&mut pair, client, server, 3)?;
            pair.client_conn_mut(client)
                .streams
                .get_mut(&3)
                .unwrap()
                .sequence_number = initial_ssn;
            pair.server_conn_mut(server)
                .streams
                .get_mut(&3)
                .unwrap()
                .reassembly_queue
                .next_ssn = initial_ssn;
            let now = pair.time;
            // Leave reliable DATA in flight so the stalled window cannot send a probe.
            pair.client_conn_mut(client)
                .open_stream(4, PayloadProtocolIdentifier::Binary)?
                .write(b"blocker")?;
            pair.client.drive(now, pair.server.addr);
            let old_cwnd = pair.client_conn_mut(client).cwnd;
            pair.client_conn_mut(client).cwnd = 7;
            pair.client_stream(client, 3)?.set_reliability_params(
                unordered,
                ReliabilityType::Timed,
                10,
            )?;
            while pair.client_conn_mut(client).poll().is_some() {}
            let old = vec![42; 3000];
            pair.client_stream(client, 3)?.write(&old)?;
            // Start the lifetime while congestion credit still prevents transmission.
            pair.client.drive(now, pair.server.addr);
            pair.client
                .drive(now + Duration::from_millis(9), pair.server.addr);
            assert_eq!(pair.client_stream(client, 3)?.buffered_amount()?, old.len());
            // Changing the stream policy cannot extend an accepted message's lifetime.
            pair.client_stream(client, 3)?.set_reliability_params(
                unordered,
                ReliabilityType::Reliable,
                0,
            )?;
            // Exact deadline expiry precedes the first TSN assignment even with a closed window.
            pair.client
                .drive(now + Duration::from_millis(10), pair.server.addr);
            assert_eq!(pair.client_stream(client, 3)?.buffered_amount()?, 0);
            pair.client
                .drive(now + Duration::from_millis(11), pair.server.addr);
            let mut low_events = 0;
            while let Some(event) = pair.client_conn_mut(client).poll() {
                if matches!(
                    event,
                    Event::Stream(StreamEvent::BufferedAmountLow { id: 3 })
                ) {
                    low_events += 1;
                }
            }
            assert_eq!(low_events, 1, "expiry releases buffer credit exactly once");
            pair.time = now + Duration::from_millis(20);
            pair.client_conn_mut(client).cwnd = old_cwnd;
            pair.client_stream(client, 3)?.set_reliability_params(
                unordered,
                ReliabilityType::Reliable,
                0,
            )?;
            pair.client_stream(client, 3)?.write(b"fresh")?;
            pair.drive();
            let chunks = pair
                .server_stream(server, 3)?
                .read()?
                .expect("fresh message");
            let mut data = vec![0; chunks.len()];
            chunks.read(&mut data)?;
            assert_eq!(data, b"fresh");
            assert!(pair.server_stream(server, 3)?.read()?.is_none());
            assert_eq!(pair.client_stream(client, 3)?.buffered_amount()?, 0);
        }
    }
    Ok(())
}

//TODO: TestAssocT1InitTimer
//TODO: TestAssocT1CookieTimer
//TODO: TestAssocT3RtxTimer

/*FIXME
// 1) Send 4 packets. drop the first one.
// 2) Last 3 packet will be received, which triggers fast-retransmission
// 3) The first one is retransmitted, which makes s1 readable
// Above should be done before RTO occurs (fast recovery)
#[test]
fn test_assoc_congestion_control_fast_retransmission() -> Result<()> {
    let _guard = subscribe();

    let si: u16 = 6;
    let mut sbuf = vec![0u8; 1000];
    for i in 0..sbuf.len() {
        sbuf[i] = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::Normal, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    //br.drop_next_nwrites(0, 1).await; // drop the first packet (second one should be sacked)

    for i in 0..4u32 {
        sbuf[0..4].copy_from_slice(&i.to_be_bytes());
        let n = pair.client_stream(client_ch, si)?.write_sctp(
            &Bytes::from(sbuf.clone()),
            PayloadProtocolIdentifier::Binary,
        )?;
        assert_eq!(sbuf.len(), n, "unexpected length of received data");
        pair.client.drive(pair.time, pair.server.addr);
        if i == 0 {
            //drop the first packet
            pair.client.outbound.clear();
        }
    }

    // process packets for 500 msec, assuming that the fast retrans/recover
    // should complete within 500 msec.
    /*for _ in 0..50 {
        br.tick().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }*/
    debug!("advance 500ms");
    pair.time += Duration::from_millis(500);
    pair.step();

    let mut buf = vec![0u8; 3000];

    // Try to read all 4 packets
    for i in 0..4 {
        {
            let q = &pair
                .server_conn_mut(server_ch)
                .streams
                .get(&si)
                .unwrap()
                .reassembly_queue;
            assert!(q.is_readable(), "should be readable at {}", i);
        }

        let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
        let (n, ppi) = (chunks.len(), chunks.ppi);
        chunks.read(&mut buf)?;
        assert_eq!(n, sbuf.len(), "unexpected length of received data");
        assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
        assert_eq!(
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
            i,
            "unexpected received data"
        );
    }

    pair.drive();
    //br.process().await;

    {
        let a = pair.client_conn_mut(client_ch);
        assert!(!a.in_fast_recovery, "should not be in fast-recovery");
        debug!("nSACKs      : {}", a.stats.get_num_sacks());
        debug!("nFastRetrans: {}", a.stats.get_num_fast_retrans());

        assert_eq!(1, a.stats.get_num_fast_retrans(), "should be 1");
    }
    {
        let b = pair.server_conn_mut(server_ch);
        debug!("nDATAs      : {}", b.stats.get_num_datas());
        debug!("nAckTimeouts: {}", b.stats.get_num_ack_timeouts());
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}*/

#[test]
fn test_assoc_congestion_control_congestion_avoidance() -> Result<()> {
    //let _guard = subscribe();

    let max_receive_buffer_size: u32 = 64 * 1024;
    let si: u16 = 6;
    let n_packets_to_send: u32 = 2000;

    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) =
        create_association_pair(AckMode::Normal, max_receive_buffer_size)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        pair.client_conn_mut(client_ch).stats.reset();
        pair.server_conn_mut(server_ch).stats.reset();
    }

    for i in 0..n_packets_to_send {
        sbuf[0..4].copy_from_slice(&i.to_be_bytes());
        let n = pair.client_stream(client_ch, si)?.write_sctp(
            &Bytes::from(sbuf.clone()),
            PayloadProtocolIdentifier::Binary,
        )?;
        assert_eq!(sbuf.len(), n, "unexpected length of received data");
    }
    pair.drive_client();
    //debug!("pair.drive_client() done");

    let mut rbuf = vec![0u8; 3000];

    // Repeat calling br.Tick() until the buffered amount becomes 0
    let mut n_packets_received = 0u32;
    while pair.client_conn_mut(client_ch).buffered_amount() > 0
        && n_packets_received < n_packets_to_send
    {
        /*println!("timestamp: {:?}", pair.time);
        println!(
            "buffered_amount {}, pair.server.inbound {}, n_packets_received {}, n_packets_to_send {}",
            pair.client_conn_mut(client_ch).buffered_amount(),
            pair.server.inbound.len(),
            n_packets_received,
            n_packets_to_send
        );*/

        pair.step();

        while let Some(chunks) = pair.server_stream(server_ch, si)?.read_sctp()? {
            let (n, ppi) = (chunks.len(), chunks.ppi);
            chunks.read(&mut rbuf)?;
            assert_eq!(sbuf.len(), n, "unexpected length of received data");
            assert_eq!(
                n_packets_received,
                u32::from_be_bytes([rbuf[0], rbuf[1], rbuf[2], rbuf[3]]),
                "unexpected length of received data"
            );
            assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

            n_packets_received += 1;
        }

        //pair.drive_client();
    }

    pair.drive();
    //println!("timestamp: {:?}", pair.time);

    assert_eq!(
        n_packets_received, n_packets_to_send,
        "unexpected num of packets received"
    );

    {
        let a = pair.client_conn_mut(client_ch);

        assert!(!a.in_fast_recovery, "should not be in fast-recovery");
        assert!(
            a.cwnd > a.ssthresh,
            "should be in congestion avoidance mode"
        );
        assert!(
            a.ssthresh >= max_receive_buffer_size,
            "{} should not be less than the initial size of 128KB {}",
            a.ssthresh,
            max_receive_buffer_size
        );

        debug!("nSACKs      : {}", a.stats.get_num_sacks());
        debug!("nT3Timeouts: {}", a.stats.get_num_t3timeouts());

        assert!(
            a.stats.get_num_sacks() <= n_packets_to_send as u64 / 2,
            "too many sacks"
        );
        assert_eq!(0, a.stats.get_num_t3timeouts(), "should be no retransmit");
    }
    {
        assert_eq!(
            0,
            pair.server_conn_mut(server_ch)
                .streams
                .get(&si)
                .unwrap()
                .get_num_bytes_in_reassembly_queue(),
            "reassembly queue should be empty"
        );

        let b = pair.server_conn_mut(server_ch);

        debug!("nDATAs      : {}", b.stats.get_num_datas());

        assert_eq!(
            n_packets_to_send as u64,
            b.stats.get_num_datas(),
            "packet count mismatch"
        );
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_congestion_control_slow_reader() -> Result<()> {
    //let _guard = subscribe();

    let max_receive_buffer_size: u32 = 64 * 1024;
    let si: u16 = 6;
    let n_packets_to_send: u32 = 130;

    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) =
        create_association_pair(AckMode::Normal, max_receive_buffer_size)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    for i in 0..n_packets_to_send {
        sbuf[0..4].copy_from_slice(&i.to_be_bytes());
        let n = pair.client_stream(client_ch, si)?.write_sctp(
            &Bytes::from(sbuf.clone()),
            PayloadProtocolIdentifier::Binary,
        )?;
        assert_eq!(sbuf.len(), n, "unexpected length of received data");
    }
    pair.drive_client();

    let mut rbuf = vec![0u8; 3000];

    // 1. First forward packets to receiver until rwnd becomes 0
    // 2. Wait until the sender's cwnd becomes 1*MTU (RTO occurred)
    // 3. Stat reading a1's data
    let mut n_packets_received = 0u32;
    let mut has_rtoed = false;
    while pair.client_conn_mut(client_ch).buffered_amount() > 0
        && n_packets_received < n_packets_to_send
    {
        /*println!(
            "buffered_amount {}, pair.server.inbound {}, n_packets_received {}, n_packets_to_send {}",
            pair.client_conn_mut(client_ch).buffered_amount(),
            pair.server.inbound.len(),
            n_packets_received,
            n_packets_to_send
        );*/

        if !has_rtoed {
            let rwnd = pair
                .server_conn_mut(server_ch)
                .get_my_receiver_window_credit();
            let cwnd = pair.client_conn_mut(client_ch).cwnd;
            let cmtu = pair.client_conn_mut(client_ch).mtu;
            if cwnd > cmtu || rwnd > 0 {
                // Do not read until a1.getMyReceiverWindowCredit() becomes zero
                pair.step();
                continue;
            }

            has_rtoed = true;
        }

        while let Some(chunks) = pair.server_stream(server_ch, si)?.read_sctp()? {
            let (n, ppi) = (chunks.len(), chunks.ppi);
            chunks.read(&mut rbuf)?;
            assert_eq!(sbuf.len(), n, "unexpected length of received data");
            assert_eq!(
                n_packets_received,
                u32::from_be_bytes([rbuf[0], rbuf[1], rbuf[2], rbuf[3]]),
                "unexpected length of received data"
            );
            assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

            n_packets_received += 1;
        }

        pair.step();
    }

    pair.drive();

    assert_eq!(
        n_packets_received, n_packets_to_send,
        "unexpected num of packets received"
    );
    assert_eq!(
        0,
        pair.server_conn_mut(server_ch)
            .streams
            .get(&si)
            .unwrap()
            .get_num_bytes_in_reassembly_queue(),
        "reassembly queue should be empty"
    );

    {
        let a = pair.client_conn_mut(client_ch);
        debug!("nSACKs      : {}", a.stats.get_num_sacks());
    }
    {
        let b = pair.server_conn_mut(server_ch);
        debug!("nDATAs      : {}", b.stats.get_num_datas());
        debug!("nAckTimeouts: {}", b.stats.get_num_ack_timeouts());
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_delayed_ack() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 6;
    let mut sbuf = vec![0u8; 1000];
    let mut rbuf = vec![0u8; 1500];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::AlwaysDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        pair.client_conn_mut(client_ch).stats.reset();
        pair.server_conn_mut(server_ch).stats.reset();
    }

    let n = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from(sbuf.clone()),
        PayloadProtocolIdentifier::Binary,
    )?;
    assert_eq!(sbuf.len(), n, "unexpected length of received data");
    pair.drive_client();

    // Repeat calling br.Tick() until the buffered amount becomes 0
    let since = pair.time;
    let mut n_packets_received = 0;
    while pair.client_conn_mut(client_ch).buffered_amount() > 0 {
        pair.step();

        while let Some(chunks) = pair.server_stream(server_ch, si)?.read_sctp()? {
            let (n, ppi) = (chunks.len(), chunks.ppi);
            chunks.read(&mut rbuf)?;
            assert_eq!(sbuf.len(), n, "unexpected length of received data");
            assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");

            n_packets_received += 1;
        }
    }
    let delay = (pair.time.duration_since(since).as_millis() as f64) / 1000.0;
    debug!("received in {} seconds", delay);
    assert!(delay >= 0.2, "should be >= 200msec");

    pair.drive();

    assert_eq!(n_packets_received, 1, "unexpected num of packets received");
    assert_eq!(
        0,
        pair.server_conn_mut(server_ch)
            .streams
            .get(&si)
            .unwrap()
            .get_num_bytes_in_reassembly_queue(),
        "reassembly queue should be empty"
    );

    let a_num_sacks = {
        let a = pair.client_conn_mut(client_ch);
        debug!("nSACKs      : {}", a.stats.get_num_sacks());
        assert_eq!(0, a.stats.get_num_t3timeouts(), "should be no retransmit");
        a.stats.get_num_sacks()
    };

    {
        let b = pair.server_conn_mut(server_ch);

        debug!("nDATAs      : {}", b.stats.get_num_datas());
        debug!("nAckTimeouts: {}", b.stats.get_num_ack_timeouts());

        assert_eq!(1, b.stats.get_num_datas(), "DATA chunk count mismatch");
        assert_eq!(
            a_num_sacks,
            b.stats.get_num_datas(),
            "sack count should be equal to the number of data chunks"
        );
        assert_eq!(
            1,
            b.stats.get_num_ack_timeouts(),
            "ackTimeout count mismatch"
        );
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reset_close_one_way() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;
    let msg: Bytes = Bytes::from_static(b"ABC");

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(0, a.buffered_amount(), "incorrect bufferedAmount");
    }

    let n = pair
        .client_stream(client_ch, si)?
        .write_sctp(&msg, PayloadProtocolIdentifier::Binary)?;
    assert_eq!(msg.len(), n, "unexpected length of received data");
    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(msg.len(), a.buffered_amount(), "incorrect bufferedAmount");
    }
    pair.step();

    let mut buf = vec![0u8; 32];

    while pair.server_stream(server_ch, si).is_ok() {
        debug!("s1.read_sctp begin");
        match pair.server_stream(server_ch, si)?.read_sctp() {
            Ok(chunks_opt) => {
                if let Some(chunks) = chunks_opt {
                    let (n, ppi) = (chunks.len(), chunks.ppi);
                    chunks.read(&mut buf)?;
                    debug!("s1.read_sctp done with {:?}", &buf[..n]);
                    assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
                    assert_eq!(n, msg.len(), "unexpected length of received data");
                }

                debug!("s0.close");
                pair.client_stream(client_ch, si)?.stop()?; // send reset

                pair.step();
            }
            Err(err) => {
                debug!("s1.read_sctp err {:?}", err);
                break;
            }
        }
    }

    pair.drive();

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_reset_close_both_ways() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;
    let msg: Bytes = Bytes::from_static(b"ABC");

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(0, a.buffered_amount(), "incorrect bufferedAmount");
    }

    let n = pair
        .client_stream(client_ch, si)?
        .write_sctp(&msg, PayloadProtocolIdentifier::Binary)?;
    assert_eq!(msg.len(), n, "unexpected length of received data");
    {
        let a = pair.client_conn_mut(client_ch);
        assert_eq!(msg.len(), a.buffered_amount(), "incorrect bufferedAmount");
    }
    pair.step();

    let mut buf = vec![0u8; 32];

    while pair.server_stream(server_ch, si).is_ok() || pair.client_stream(client_ch, si).is_ok() {
        if pair.server_stream(server_ch, si).is_ok() {
            debug!("s1.read_sctp begin");
            match pair.server_stream(server_ch, si)?.read_sctp() {
                Ok(chunks_opt) => {
                    if let Some(chunks) = chunks_opt {
                        let (n, ppi) = (chunks.len(), chunks.ppi);
                        chunks.read(&mut buf)?;
                        debug!("s1.read_sctp done with {:?}", &buf[..n]);
                        assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
                        assert_eq!(n, msg.len(), "unexpected length of received data");
                    }
                }
                Err(err) => {
                    debug!("s1.read_sctp err {:?}", err);
                    break;
                }
            }
        }

        if pair.client_stream(client_ch, si).is_ok() {
            debug!("s0.read_sctp begin");
            match pair.client_stream(client_ch, si)?.read_sctp() {
                Ok(chunks_opt) => {
                    if let Some(chunks) = chunks_opt {
                        let (n, ppi) = (chunks.len(), chunks.ppi);
                        chunks.read(&mut buf)?;
                        debug!("s0.read_sctp done with {:?}", &buf[..n]);
                        assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
                        assert_eq!(n, msg.len(), "unexpected length of received data");
                    }
                }
                Err(err) => {
                    debug!("s0.read_sctp err {:?}", err);
                    break;
                }
            }
        }

        if pair.client_stream(client_ch, si).is_ok() {
            pair.client_stream(client_ch, si)?.stop()?; // send reset
        }
        if pair.server_stream(server_ch, si).is_ok() {
            pair.server_stream(server_ch, si)?.stop()?; // send reset
        }

        pair.step();
    }

    pair.drive();

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_assoc_abort() -> Result<()> {
    //let _guard = subscribe();

    let si: u16 = 1;

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;

    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    let transmit = {
        let abort = ChunkAbort {
            error_causes: vec![ErrorCauseProtocolViolation {
                code: PROTOCOL_VIOLATION,
                ..Default::default()
            }],
        };

        let packet = pair
            .client_conn_mut(client_ch)
            .create_packet(vec![Box::new(abort)])
            .marshal()?;

        Transmit {
            now: pair.time,
            remote: pair.server.addr,
            ecn: None,
            local_ip: None,
            payload: Payload::RawEncode(vec![packet]),
        }
    };

    // Both associations are established
    assert_eq!(
        AssociationState::Established,
        pair.client_conn_mut(client_ch).state()
    );
    assert_eq!(
        AssociationState::Established,
        pair.server_conn_mut(server_ch).state()
    );

    debug!("send ChunkAbort");
    pair.client.outbound.push_back(transmit);

    pair.drive();

    // The receiving association should be closed because it got an ABORT
    assert_eq!(
        AssociationState::Established,
        pair.client_conn_mut(client_ch).state()
    );
    assert_eq!(
        AssociationState::Closed,
        pair.server_conn_mut(server_ch).state()
    );

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

#[test]
fn test_association_handle_packet_before_init() -> Result<()> {
    //let _guard = subscribe();

    let tests = vec![
        (
            "InitAck",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::new(ChunkInit {
                    is_ack: true,
                    initiate_tag: 1,
                    num_inbound_streams: 1,
                    num_outbound_streams: 1,
                    advertised_receiver_window_credit: 1500,
                    ..Default::default()
                })],
            },
        ),
        (
            "Abort",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::<ChunkAbort>::default()],
            },
        ),
        (
            "CoockeEcho",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::<ChunkCookieEcho>::default()],
            },
        ),
        (
            "HeartBeat",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::<ChunkHeartbeat>::default()],
            },
        ),
        (
            "PayloadData",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::<ChunkPayloadData>::default()],
            },
        ),
        (
            "Sack",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::new(ChunkSelectiveAck {
                    cumulative_tsn_ack: 1000,
                    advertised_receiver_window_credit: 1500,
                    gap_ack_blocks: vec![GapAckBlock {
                        start: 100,
                        end: 200,
                    }],
                    ..Default::default()
                })],
            },
        ),
        (
            "Reconfig",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::new(ChunkReconfig {
                    param_a: Some(Box::<ParamOutgoingResetRequest>::default()),
                    param_b: Some(Box::<ParamReconfigResponse>::default()),
                })],
            },
        ),
        (
            "ForwardTSN",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::new(ChunkForwardTsn {
                    new_cumulative_tsn: 100,
                    ..Default::default()
                })],
            },
        ),
        (
            "Error",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::<ChunkError>::default()],
            },
        ),
        (
            "Shutdown",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::<ChunkShutdown>::default()],
            },
        ),
        (
            "ShutdownAck",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::new(ChunkShutdownAck)],
            },
        ),
        (
            "ShutdownComplete",
            Packet {
                common_header: CommonHeader {
                    source_port: 1,
                    destination_port: 1,
                    verification_tag: 0,
                },
                chunks: vec![Box::new(ChunkShutdownComplete)],
            },
        ),
    ];

    let remote = SocketAddr::from_str("0.0.0.0:0").unwrap();

    for (name, packet) in tests {
        debug!("testing {}", name);

        //let (a_conn, charlie_conn) = pipe();
        let config = Arc::new(TransportConfig::default());
        let mut a = Association::new(None, config, 1400, 0, remote, None, Instant::now());

        let packet = packet.marshal()?;
        a.handle_event(AssociationEvent(AssociationEventInner::Datagram(
            Transmit {
                now: Instant::now(),
                remote,
                ecn: None,
                local_ip: None,
                payload: Payload::RawEncode(vec![packet]),
            },
        )));

        a.close()?;
    }

    Ok(())
}

// This test reproduces an issue related to having regular messages (regular acks) which keep
// rescheduling the T3RTX timer before it can ever fire.
#[test]
fn test_old_rtx_on_regular_acks() -> Result<()> {
    let si: u16 = 6;
    let mut sbuf = vec![0u8; 1000];
    for (i, b) in sbuf.iter_mut().enumerate() {
        *b = (i & 0xff) as u8;
    }

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::Normal, 0)?;
    pair.latency = Duration::from_millis(500);
    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // Send 20 packet at a regular interval that is < RTO
    for i in 0..20u32 {
        println!("sending packet {}", i);
        sbuf[0..4].copy_from_slice(&i.to_be_bytes());
        let n = pair.client_stream(client_ch, si)?.write_sctp(
            &Bytes::from(sbuf.clone()),
            PayloadProtocolIdentifier::Binary,
        )?;
        assert_eq!(sbuf.len(), n, "unexpected length of received data");
        pair.client.drive(pair.time, pair.server.addr);

        // drop a few transmits
        if (5..10).contains(&i) {
            pair.client.outbound.clear();
        }

        pair.drive_client();
        pair.drive_server();
        pair.time += Duration::from_millis(500);
    }

    pair.drive_client();
    pair.drive_server();

    let mut buf = vec![0u8; 3000];

    // All packets must readable correctly
    for i in 0..20 {
        {
            let q = &pair
                .server_conn_mut(server_ch)
                .streams
                .get(&si)
                .unwrap()
                .reassembly_queue;
            println!("q.is_readable()={}", q.is_readable());
            assert!(q.is_readable(), "should be readable at {}", i);
        }

        let chunks = pair.server_stream(server_ch, si)?.read_sctp()?.unwrap();
        let (n, ppi) = (chunks.len(), chunks.ppi);
        chunks.read(&mut buf)?;
        assert_eq!(n, sbuf.len(), "unexpected length of received data");
        assert_eq!(ppi, PayloadProtocolIdentifier::Binary, "unexpected ppi");
        assert_eq!(
            u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]),
            i,
            "unexpected received data"
        );
    }

    close_association_pair(&mut pair, client_ch, server_ch, si);

    Ok(())
}

/*
TODO: The following tests will be moved to sctp-async tests:
struct FakeEchoConn {
    wr_tx: Mutex<mpsc::Sender<Vec<u8>>>,
    rd_rx: Mutex<mpsc::Receiver<Vec<u8>>>,
    bytes_sent: AtomicUsize,
    bytes_received: AtomicUsize,
}

impl FakeEchoConn {
    fn new() -> impl Conn + AsAny {
        let (wr_tx, rd_rx) = mpsc::channel(1);
        FakeEchoConn {
            wr_tx: Mutex::new(wr_tx),
            rd_rx: Mutex::new(rd_rx),
            bytes_sent: AtomicUsize::new(0),
            bytes_received: AtomicUsize::new(0),
        }
    }
}

trait AsAny {
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync);
}

impl AsAny for FakeEchoConn {
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

type UResult<T> = std::result::Result<T, util::Error>;

#[async_trait]
impl Conn for FakeEchoConn {
    fn connect(&self, _addr: SocketAddr) -> UResult<()> {
        Err(io::Error::new(io::ErrorKind::Other, "Not applicable").into())
    }

    fn recv(&self, b: &mut [u8]) -> UResult<usize> {
        let mut rd_rx = self.rd_rx.lock().await;
        let v = match rd_rx.recv().await {
            Some(v) => v,
            None => {
                return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Unexpected EOF").into())
            }
        };
        let l = std::cmp::min(v.len(), b.len());
        b[..l].copy_from_slice(&v[..l]);
        self.bytes_received.fetch_add(l, Ordering::SeqCst);
        Ok(l)
    }

    fn recv_from(&self, _buf: &mut [u8]) -> UResult<(usize, SocketAddr)> {
        Err(io::Error::new(io::ErrorKind::Other, "Not applicable").into())
    }

    fn send(&self, b: &[u8]) -> UResult<usize> {
        let wr_tx = self.wr_tx.lock().await;
        match wr_tx.send(b.to_vec()).await {
            Ok(_) => {}
            Err(err) => return Err(io::Error::new(io::ErrorKind::Other, err.to_string()).into()),
        };
        self.bytes_sent.fetch_add(b.len(), Ordering::SeqCst);
        Ok(b.len())
    }

    fn send_to(&self, _buf: &[u8], _target: SocketAddr) -> UResult<usize> {
        Err(io::Error::new(io::ErrorKind::Other, "Not applicable").into())
    }

    fn local_addr(&self) -> UResult<SocketAddr> {
        Err(io::Error::new(io::ErrorKind::AddrNotAvailable, "Addr Not Available").into())
    }

    fn remote_addr(&self) -> Option<SocketAddr> {
        None
    }

    fn close(&self) -> UResult<()> {
        Ok(())
    }
}

//use std::io::Write;

#[test]
fn test_stats() -> Result<()> {
    /*env_logger::Builder::new()
    .format(|buf, record| {
        writeln!(
            buf,
            "{}:{} [{}] {} - {}",
            record.file().unwrap_or("unknown"),
            record.line().unwrap_or(0),
            record.level(),
            chrono::Local::now().format("%H:%M:%S.%6f"),
            record.args()
        )
    })
    .filter(None, log::LevelFilter::Trace)
    .init();*/

    let conn = Arc::new(FakeEchoConn::new());
    let a = Association::client(Config {
        net_conn: Arc::clone(&conn) as Arc<dyn Conn + Send + Sync>,
        max_receive_buffer_size: 0,
        max_message_size: 0,
        name: "client".to_owned(),
    })
    .await?;

    if let Some(conn) = conn.as_any().downcast_ref::<FakeEchoConn>() {
        assert_eq!(
            conn.bytes_received.load(Ordering::SeqCst),
            a.bytes_received()
        );
        assert_eq!(conn.bytes_sent.load(Ordering::SeqCst), a.bytes_sent());
    } else {
        assert!(false, "must be FakeEchoConn");
    }

    Ok(())
}

fn create_assocs() -> Result<(Association, Association)> {
    let addr1 = SocketAddr::from_str("0.0.0.0:0").unwrap();
    let addr2 = SocketAddr::from_str("0.0.0.0:0").unwrap();

    let udp1 = UdpSocket::bind(addr1).await.unwrap();
    let udp2 = UdpSocket::bind(addr2).await.unwrap();

    udp1.connect(udp2.local_addr().unwrap()).await.unwrap();
    udp2.connect(udp1.local_addr().unwrap()).await.unwrap();

    let (a1chan_tx, mut a1chan_rx) = mpsc::channel(1);
    let (a2chan_tx, mut a2chan_rx) = mpsc::channel(1);

    tokio::spawn(async move {
        let a = Association::client(Config {
            net_conn: Arc::new(udp1),
            max_receive_buffer_size: 0,
            max_message_size: 0,
            name: "client".to_owned(),
        })
        .await?;

        let _ = a1chan_tx.send(a).await;

        Result::<()>::Ok(())
    });

    tokio::spawn(async move {
        let a = Association::server(Config {
            net_conn: Arc::new(udp2),
            max_receive_buffer_size: 0,
            max_message_size: 0,
            name: "server".to_owned(),
        })
        .await?;

        let _ = a2chan_tx.send(a).await;

        Result::<()>::Ok(())
    });

    let timer1 = tokio::time::sleep(Duration::from_secs(1));
    tokio::pin!(timer1);
    let a1 = tokio::select! {
        _ = timer1.as_mut() =>{
            assert!(false,"timed out waiting for a1");
            return Err(Error::Other("timed out waiting for a1".to_owned()).into());
        },
        a1 = a1chan_rx.recv() => {
            a1.unwrap()
        }
    };

    let timer2 = tokio::time::sleep(Duration::from_secs(1));
    tokio::pin!(timer2);
    let a2 = tokio::select! {
        _ = timer2.as_mut() =>{
            assert!(false,"timed out waiting for a2");
            return Err(Error::Other("timed out waiting for a2".to_owned()).into());
        },
        a2 = a2chan_rx.recv() => {
            a2.unwrap()
        }
    };

    Ok((a1, a2))
}

//use std::io::Write;
//TODO: remove this conditional test
#[cfg(not(target_os = "windows"))]
#[test]
fn test_association_shutdown() -> Result<()> {
    /*env_logger::Builder::new()
    .format(|buf, record| {
        writeln!(
            buf,
            "{}:{} [{}] {} - {}",
            record.file().unwrap_or("unknown"),
            record.line().unwrap_or(0),
            record.level(),
            chrono::Local::now().format("%H:%M:%S.%6f"),
            record.args()
        )
    })
    .filter(None, log::LevelFilter::Trace)
    .init();*/

    let (a1, a2) = create_assocs().await?;

    let s11 = a1.open_stream(1, PayloadProtocolIdentifier::String).await?;
    let s21 = a2.open_stream(1, PayloadProtocolIdentifier::String).await?;

    let test_data = Bytes::from_static(b"test");

    let n = s11.write(&test_data).await?;
    assert_eq!(test_data.len(), n);

    let mut buf = vec![0u8; test_data.len()];
    let n = s21.read(&mut buf).await?;
    assert_eq!(test_data.len(), n);
    assert_eq!(&test_data, &buf[0..n]);

    if let Ok(result) = tokio::time::timeout(Duration::from_secs(1), a1.shutdown()).await {
        assert!(result.is_ok(), "shutdown should be ok");
    } else {
        assert!(false, "shutdown timeout");
    }

    {
        let mut close_loop_ch_rx = a2.close_loop_ch_rx.lock().await;

        // Wait for close read loop channels to prevent flaky tests.
        let timer2 = tokio::time::sleep(Duration::from_secs(1));
        tokio::pin!(timer2);
        tokio::select! {
            _ = timer2.as_mut() =>{
                assert!(false,"timed out waiting for a2 read loop to close");
            },
            _ = close_loop_ch_rx.recv() => {
                log::debug!("recv a2.close_loop_ch_rx");
            }
        };
    }
    Ok(())
}


fn test_association_shutdown_during_write() -> Result<()> {
    //let _guard = subscribe();

    let (a1, a2) = create_assocs().await?;

    let s11 = a1.open_stream(1, PayloadProtocolIdentifier::String).await?;
    let s21 = a2.open_stream(1, PayloadProtocolIdentifier::String).await?;

    let (writing_done_tx, mut writing_done_rx) = mpsc::channel::<()>(1);
    let ss21 = Arc::clone(&s21);
    tokio::spawn(async move {
        let mut i = 0;
        while ss21.write(&Bytes::from(vec![i])).await.is_ok() {
            if i == 255 {
                i = 0;
            } else {
                i += 1;
            }

            if i % 100 == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }

        drop(writing_done_tx);
    });

    let test_data = Bytes::from_static(b"test");

    let n = s11.write(&test_data).await?;
    assert_eq!(test_data.len(), n);

    let mut buf = vec![0u8; test_data.len()];
    let n = s21.read(&mut buf).await?;
    assert_eq!(test_data.len(), n);
    assert_eq!(&test_data, &buf[0..n]);

    {
        let mut close_loop_ch_rx = a1.close_loop_ch_rx.lock().await;
        tokio::select! {
            res = tokio::time::timeout(Duration::from_secs(1), a1.shutdown()) => {
                if let Ok(result) = res {
                    assert!(result.is_ok(), "shutdown should be ok");
                } else {
                    assert!(false, "shutdown timeout");
                }
            }
            _ = writing_done_rx.recv() => {
                log::debug!("writing_done_rx");
                let result = close_loop_ch_rx.recv().await;
                log::debug!("a1.close_loop_ch_rx.recv: {:?}", result);
            },
        };
    }

    {
        let mut close_loop_ch_rx = a2.close_loop_ch_rx.lock().await;
        // Wait for close read loop channels to prevent flaky tests.
        let timer2 = tokio::time::sleep(Duration::from_secs(1));
        tokio::pin!(timer2);
        tokio::select! {
            _ = timer2.as_mut() =>{
                assert!(false,"timed out waiting for a2 read loop to close");
            },
            _ = close_loop_ch_rx.recv() => {
                log::debug!("recv a2.close_loop_ch_rx");
            }
        };
    }

    Ok(())
}*/

#[test]
fn test_assoc_reset_duplicate_reconfig_request() -> Result<()> {
    let si: u16 = 1;

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;
    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // Client initiates reset of stream 1
    pair.client_stream(client_ch, si)?.stop()?;

    // Drive client to generate the reconfig packet, which lands in server's inbound
    pair.drive_client();

    // Capture the raw packet bytes before the server processes them.
    // These contain the ChunkReconfig with the ParamOutgoingResetRequest.
    let captured_packets: Vec<_> = pair.server.inbound.iter().cloned().collect();

    // Let the entire reset flow complete
    pair.drive();

    // Verify stream 1 is gone on the server
    assert!(
        pair.server_stream(server_ch, si).is_err(),
        "stream 1 should be removed after reset"
    );

    // Server opens a new stream 1
    let _ = pair
        .server_conn_mut(server_ch)
        .open_stream(si, PayloadProtocolIdentifier::Binary)?;
    assert!(
        pair.server_stream(server_ch, si).is_ok(),
        "new stream 1 should exist"
    );

    // Inject the captured reconfig packets again (simulating retransmission)
    for packet in captured_packets {
        pair.server.inbound.push_back(packet);
    }

    // Process the injected packets
    pair.drive();

    // The new stream 1 must NOT be destroyed by the duplicate reconfig request
    assert!(
        pair.server_stream(server_ch, si).is_ok(),
        "new stream 1 should NOT be destroyed by duplicate reconfig request"
    );

    Ok(())
}

/// Verify that a retransmission of a RE-CONFIG request that was initially
/// InProgress and then completed via TSN advance (not via handle_reconfig_param)
/// does not destroy a reused stream. This exercises the case where
/// max_completed_reconfig_rsn must be updated in the TSN-advance loop.
#[test]
fn test_assoc_reset_inprogress_completed_via_tsn_advance_then_retransmit() -> Result<()> {
    let si: u16 = 1;

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;
    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // Write data so my_next_tsn advances on the client; the RECONFIG
    // sender_last_tsn will include this TSN.
    let _ = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from_static(b"payload"),
        PayloadProtocolIdentifier::Binary,
    )?;

    // Drive client to generate the DATA packet(s).
    pair.drive_client();

    // Withhold the DATA packets from the server (simulate loss).
    let withheld: Vec<_> = pair.server.inbound.drain(..).collect();

    // Client initiates reset of stream 1.
    pair.client_stream(client_ch, si)?.stop()?;

    // Drive client to generate EOS data + RECONFIG packets.
    pair.drive_client();

    // Separate RECONFIG-bearing packets from DATA-only packets.
    let (reconfig_packets, data_packets): (Vec<_>, Vec<_>) =
        pair.server.inbound.drain(..).partition(|(_, _, raw)| {
            let mut offset = 12usize;
            while offset + 4 <= raw.len() {
                if raw[offset] == 130 {
                    return true;
                }
                let chunk_len = u16::from_be_bytes([raw[offset + 2], raw[offset + 3]]) as usize;
                if chunk_len < 4 {
                    break;
                }
                offset += (chunk_len + 3) & !3;
            }
            false
        });

    assert!(!reconfig_packets.is_empty(), "expected a RECONFIG packet");

    // Step 1: Deliver only the RECONFIG. Server hasn't seen the withheld
    // DATA so peer_last_tsn < sender_last_tsn → InProgress.
    for pkt in &reconfig_packets {
        pair.server.inbound.push_back(pkt.clone());
    }
    pair.drive_server();

    assert!(
        pair.server_stream(server_ch, si).is_ok(),
        "stream should survive InProgress reset"
    );

    // Step 2: Now deliver the withheld DATA + the other data packets.
    // The TSN-advance loop in handle_data will complete the pending
    // reconfig request (removing it from reconfig_requests).
    // Crucially, this does NOT go through handle_reconfig_param, so
    // max_completed_reconfig_rsn is only updated if the TSN-advance
    // path does it.
    for pkt in withheld {
        pair.server.inbound.push_back(pkt);
    }
    for pkt in data_packets {
        pair.server.inbound.push_back(pkt);
    }
    pair.drive();

    // The boundary DATA must remain readable even though the reset completed.
    // Draining it retires the old stream generation.
    let boundary_data = pair
        .server_stream(server_ch, si)?
        .read()?
        .expect("boundary DATA should survive the reset");
    assert_eq!(boundary_data.len(), b"payload".len());

    assert!(
        pair.server_stream(server_ch, si).is_err(),
        "stream should retire after its boundary DATA is drained"
    );

    // Step 3: Reopen stream 1 with the same ID.
    let _ = pair
        .server_conn_mut(server_ch)
        .open_stream(si, PayloadProtocolIdentifier::Binary)?;
    assert!(
        pair.server_stream(server_ch, si).is_ok(),
        "new stream 1 should exist"
    );

    // Step 4: Replay the original RECONFIG (simulating a late retransmission).
    // If the watermark was not updated during the TSN-advance completion,
    // this will bypass the dedup guard and destroy the new stream.
    for pkt in reconfig_packets {
        pair.server.inbound.push_back(pkt);
    }
    pair.drive();

    // The new stream 1 must survive.
    assert!(
        pair.server_stream(server_ch, si).is_ok(),
        "new stream 1 should NOT be destroyed by retransmitted reconfig \
         after InProgress completion via TSN advance"
    );

    Ok(())
}

/// Verify that a retransmission of an InProgress RE-CONFIG request is
/// re-evaluated (not falsely deduplicated) so the reset completes once
/// the outstanding TSN is finally received.
#[test]
fn test_assoc_reset_inprogress_reconfig_retransmission() -> Result<()> {
    let si: u16 = 1;

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;
    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // Write data so my_next_tsn advances on the client; the RECONFIG
    // sender_last_tsn will include this TSN.
    let _ = pair.client_stream(client_ch, si)?.write_sctp(
        &Bytes::from_static(b"payload"),
        PayloadProtocolIdentifier::Binary,
    )?;

    // Drive client to generate the DATA packet(s).
    pair.drive_client();

    // Withhold the DATA packets from the server (simulate loss).
    let withheld: Vec<_> = pair.server.inbound.drain(..).collect();

    // Client initiates reset of stream 1.
    pair.client_stream(client_ch, si)?.stop()?;

    // Drive client to generate EOS data + RECONFIG packets.
    pair.drive_client();

    // Separate RECONFIG-bearing packets from DATA-only packets.
    // Chunk type is the first byte after the 12-byte common header.
    let (reconfig_packets, data_packets): (Vec<_>, Vec<_>) =
        pair.server.inbound.drain(..).partition(|(_, _, raw)| {
            // Scan all chunks in the packet for CT_RECONFIG (130).
            let mut offset = 12usize;
            while offset + 4 <= raw.len() {
                if raw[offset] == 130 {
                    return true;
                }
                let chunk_len = u16::from_be_bytes([raw[offset + 2], raw[offset + 3]]) as usize;
                if chunk_len < 4 {
                    break;
                }
                offset += (chunk_len + 3) & !3; // pad to 4-byte boundary
            }
            false
        });

    assert!(!reconfig_packets.is_empty(), "expected a RECONFIG packet");

    // Deliver only the RECONFIG packets. The server hasn't seen the withheld
    // DATA so peer_last_tsn < sender_last_tsn → InProgress.
    for pkt in &reconfig_packets {
        pair.server.inbound.push_back(pkt.clone());
    }
    pair.drive_server();

    // Stream should still exist (reset is InProgress, not completed).
    assert!(
        pair.server_stream(server_ch, si).is_ok(),
        "stream should survive InProgress reset"
    );

    // Inject the RECONFIG again (simulating retransmission) together with
    // the withheld DATA so the TSN can finally advance.
    for pkt in reconfig_packets {
        pair.server.inbound.push_back(pkt);
    }
    for pkt in withheld {
        pair.server.inbound.push_back(pkt);
    }
    for pkt in data_packets {
        pair.server.inbound.push_back(pkt);
    }

    // Let everything settle.
    pair.drive();

    // The boundary DATA must remain readable even though the reset completed.
    // Draining it retires the old stream generation.
    let boundary_data = pair
        .server_stream(server_ch, si)?
        .read()?
        .expect("boundary DATA should survive the reset");
    assert_eq!(boundary_data.len(), b"payload".len());

    assert!(
        pair.server_stream(server_ch, si).is_err(),
        "stream should retire after its boundary DATA is drained"
    );

    Ok(())
}

#[test]
fn test_assoc_open_stream_rejects_pending_reset_id() -> Result<()> {
    let si: u16 = 1;

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;
    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // SERVER initiates reset of stream 1. When the client processes the server's
    // incoming RE-CONFIG, it removes stream 1 from self.streams and generates its
    // own outgoing RE-CONFIG (stored in self.reconfigs) awaiting acknowledgment.
    pair.server_stream(server_ch, si)?.stop()?;

    // Drive server to generate and deliver the RE-CONFIG to the client's inbound
    pair.drive_server();

    // Drive client to process the server's RE-CONFIG. This removes stream 1 from
    // the client's stream table and creates a pending outgoing RE-CONFIG.
    pair.drive_client();

    // Client's stream 1 is removed, but the outgoing RE-CONFIG is still pending.
    // open_stream should reject this stream ID.
    assert!(
        pair.client_stream(client_ch, si).is_err(),
        "stream 1 should be removed from client"
    );
    match pair
        .client_conn_mut(client_ch)
        .open_stream(si, PayloadProtocolIdentifier::Binary)
    {
        Err(Error::ErrStreamResetPending) => {}
        other => panic!("expected ErrStreamResetPending, got {:?}", other.err()),
    }

    // Let the full RE-CONFIG exchange complete
    pair.drive();

    // Now the RE-CONFIG is acknowledged, stream 1 should be available for reuse
    let _ = pair
        .client_conn_mut(client_ch)
        .open_stream(si, PayloadProtocolIdentifier::Binary)?;
    assert!(
        pair.client_stream(client_ch, si).is_ok(),
        "stream 1 should be available after RE-CONFIG is acknowledged"
    );

    Ok(())
}

#[test]
fn test_assoc_reconfig_failure_keeps_stream_quarantined() -> Result<()> {
    let si: u16 = 1;

    let (mut pair, client_ch, server_ch) = create_association_pair(AckMode::NoDelay, 0)?;
    establish_session_pair(&mut pair, client_ch, server_ch, si)?;

    // SERVER initiates reset of stream 1. When the client processes the incoming
    // RE-CONFIG it removes stream 1 from self.streams and generates its own
    // outgoing RE-CONFIG (stored in self.reconfigs).
    pair.server_stream(server_ch, si)?.stop()?;

    // Drive server to send the RE-CONFIG to client
    pair.drive_server();

    // Drive client to process the incoming RE-CONFIG
    pair.drive_client();

    // Drop any packets the client sent so the server never ACKs the client's
    // outgoing RE-CONFIG.
    pair.server.inbound.clear();

    // Verify open_stream is blocked by the pending outgoing RE-CONFIG
    match pair
        .client_conn_mut(client_ch)
        .open_stream(si, PayloadProtocolIdentifier::Binary)
    {
        Err(Error::ErrStreamResetPending) => {}
        Err(e) => panic!("expected ErrStreamResetPending, got Err({:?})", e),
        Ok(_) => panic!("expected ErrStreamResetPending, got Ok"),
    }

    // Advance time through MAX_INIT_RETRANS (8) retransmissions + 1 to trigger
    // failure. Each iteration jumps 61 seconds (> RTO_MAX of 60s) to guarantee
    // the Reconfig timer fires every time. We discard all outbound packets so
    // the Reconfig is never acknowledged.
    for _ in 0..10 {
        pair.time += Duration::from_secs(61);
        pair.drive_client();
        pair.server.inbound.clear();
    }

    assert!(
        core::iter::from_fn(|| pair.client_conn_mut(client_ch).poll()).any(|event| matches!(
            event,
            Event::Stream(StreamEvent::ResetFailed {
                id,
                reason: StreamResetError::Failed,
            }) if id == si
        )),
        "reset retransmission exhaustion should emit a terminal failure event"
    );

    // The in-flight request is abandoned, but without a terminal success the
    // stream ID remains quarantined and must not be reused.
    assert!(matches!(
        pair.client_conn_mut(client_ch)
            .open_stream(si, PayloadProtocolIdentifier::Binary),
        Err(Error::ErrStreamResetPending)
    ));

    Ok(())
}

#[test]
fn test_configured_sctp_ports_complete_handshake() -> Result<()> {
    let mut pair = Pair::default();
    let config = ClientConfig {
        transport: Arc::new(
            TransportConfig::default()
                .with_local_port(41234)
                .with_remote_port(9899),
        ),
        ..Default::default()
    };

    let (client_ch, server_ch) = pair.connect_with(config);
    establish_session_pair(&mut pair, client_ch, server_ch, 1)
}

#[test]
fn test_connect_rejects_zero_sctp_port() {
    let remote: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    for (local_port, remote_port) in [(0, 5000), (5000, 0)] {
        for use_snap in [false, true] {
            let transport = TransportConfig::default()
                .with_local_port(local_port)
                .with_remote_port(remote_port);
            let mut config = ClientConfig {
                transport: Arc::new(transport),
                ..Default::default()
            };
            if use_snap {
                let local_init = generate_snap_token(&config.transport).unwrap();
                let remote_init = generate_snap_token(&config.transport).unwrap();
                config = config.with_snap(local_init, remote_init);
            }

            let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
            assert_matches!(
                endpoint.connect(config, remote),
                Err(ConnectError::InvalidSctpPort)
            );
        }
    }
}

#[test]
fn test_snap_connect_established_and_transmit_uses_peer_verification_tag() {
    let now = Instant::now();

    let local_transport = TransportConfig::default();
    let remote_transport = TransportConfig::default();

    let local_init_bytes = generate_snap_token(&local_transport).expect("generate local init");
    let remote_init_bytes = generate_snap_token(&remote_transport).expect("generate remote init");

    // Parse remote to check verification tag later
    let remote_init =
        ChunkInit::unmarshal(&remote_init_bytes).expect("unmarshal remote INIT chunk");

    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let client_config = ClientConfig::new().with_snap(local_init_bytes, remote_init_bytes);
    let (_ch, mut assoc) = endpoint
        .connect(client_config, remote_addr)
        .expect("SNAP connect should succeed");

    assert_eq!(assoc.state(), AssociationState::Established);
    assert_matches!(assoc.poll(), Some(Event::Connected));

    // Ensure outbound packets use the peer's initiate_tag as the verification tag.
    let mut stream = assoc
        .open_stream(1, PayloadProtocolIdentifier::Binary)
        .expect("open stream");
    let msg = Bytes::from_static(b"hello");
    stream
        .write_sctp(&msg, PayloadProtocolIdentifier::Binary)
        .expect("write_sctp");

    let transmit = assoc
        .poll_transmit(now)
        .expect("expected at least one outbound datagram");
    let Payload::RawEncode(datagrams) = transmit.payload else {
        panic!("expected RawEncode transmit");
    };
    assert!(
        !datagrams.is_empty(),
        "expected at least one outbound packet"
    );

    let pkt = Packet::unmarshal(&datagrams[0]).expect("unmarshal outbound packet");
    assert_eq!(
        pkt.common_header.verification_tag, remote_init.initiate_tag,
        "outbound packet should use peer initiate_tag as verification_tag"
    );
}

#[test]
fn test_server_accepts_init_with_supported_address_types() {
    let now = Instant::now();
    let mut endpoint = Endpoint::new(
        Arc::new(EndpointConfig::default()),
        Some(Arc::new(ServerConfig::default())),
    );
    let remote: SocketAddr = "127.0.0.1:5000".parse().unwrap();
    let init = ChunkInit {
        initiate_tag: 0x1122_3344,
        advertised_receiver_window_credit: 1500,
        num_outbound_streams: 1,
        num_inbound_streams: 1,
        initial_tsn: 1,
        params: vec![Box::new(ParamSupportedAddressTypes {
            address_types: vec![ParamType::Ipv4Addr, ParamType::Ipv6Addr],
        })],
        ..Default::default()
    };
    let packet = Packet {
        common_header: CommonHeader {
            source_port: 5000,
            destination_port: 5000,
            verification_tag: 0,
        },
        chunks: vec![Box::new(init)],
    };

    let (_, DatagramEvent::NewAssociation(mut association)) = endpoint
        .handle(now, remote, None, None, packet.marshal().unwrap())
        .expect("INIT should create an association")
    else {
        panic!("expected a new association");
    };

    let transmit = association
        .poll_transmit(now)
        .expect("server should respond to INIT");
    let Payload::RawEncode(datagrams) = transmit.payload else {
        panic!("expected encoded INIT ACK");
    };
    let response = Packet::unmarshal(&datagrams[0]).expect("valid INIT ACK packet");
    let init_ack = response.chunks[0]
        .as_any()
        .downcast_ref::<ChunkInit>()
        .expect("server should send INIT ACK");
    assert!(init_ack.is_ack);
}

#[test]
fn test_server_retransmitted_init_routes_to_existing_association() {
    let now = Instant::now();

    let mut endpoint = Endpoint::new(
        Arc::new(EndpointConfig::default()),
        Some(Arc::new(ServerConfig::default())),
    );

    let remote: SocketAddr = "127.0.0.1:5000".parse().unwrap();
    let init_tag: u32 = 0x1122_3344;

    let init = ChunkInit {
        is_ack: false,
        initiate_tag: init_tag,
        ..Default::default()
    };

    let pkt = Packet {
        common_header: CommonHeader {
            source_port: 5000,
            destination_port: 5000,
            verification_tag: 0,
        },
        chunks: vec![Box::new(init)],
    };

    let bytes = pkt.marshal().expect("marshal INIT packet");

    let (ch1, ev1) = endpoint
        .handle(now, remote, None, None, bytes.clone())
        .expect("first INIT should be handled");
    assert!(
        matches!(ev1, DatagramEvent::NewAssociation(_)),
        "first INIT should create a new association"
    );

    // Same INIT retransmitted: should route to the same association handle.
    let (ch2, ev2) = endpoint
        .handle(now, remote, None, None, bytes)
        .expect("retransmitted INIT should be handled");
    assert_eq!(
        ch2, ch1,
        "retransmitted INIT should map to same association"
    );
    assert!(
        matches!(ev2, DatagramEvent::AssociationEvent(_)),
        "retransmitted INIT should be routed to existing association"
    );
}

#[test]
fn test_snap_rejects_invalid_remote_bytes() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let client_config = ClientConfig::new().with_snap(local_init, Bytes::from_static(b"nope"));
    let res = endpoint.connect(client_config, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "invalid remote bytes should fail with ParseFailed"
    );
}

#[test]
fn test_snap_rejects_invalid_local_bytes() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let remote_init = generate_snap_token(&TransportConfig::default()).expect("remote init");
    let client_config = ClientConfig::new().with_snap(Bytes::from_static(b"nope"), remote_init);
    let res = endpoint.connect(client_config, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Local,
                ..
            }))
        ),
        "invalid local bytes should fail with ParseFailed"
    );
}

#[test]
fn test_snap_rejects_remote_init_ack() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let remote_init_ack = ChunkInit {
        is_ack: true,
        initiate_tag: 1,
        ..Default::default()
    };
    let remote_bytes = remote_init_ack.marshal().expect("marshal remote INIT-ACK");

    let client_config = ClientConfig::new().with_snap(local_init, remote_bytes);
    let res = endpoint.connect(client_config, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::InvalidInitAck {
                side: SnapSide::Remote
            }))
        ),
        "remote INIT-ACK should be rejected"
    );
}

#[test]
fn test_snap_rejects_remote_init_with_zero_initiate_tag() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let remote_init = ChunkInit {
        is_ack: false,
        initiate_tag: 0,
        ..Default::default()
    };
    let remote_bytes = remote_init.marshal().expect("marshal remote INIT");

    let client_config = ClientConfig::new().with_snap(local_init, remote_bytes);
    let res = endpoint.connect(client_config, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ZeroInitiateTag {
                side: SnapSide::Remote
            }))
        ),
        "remote INIT with initiate_tag=0 should be rejected"
    );
}

#[test]
fn test_snap_partial_config_falls_back_to_handshake() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    // Only local_sctp_init set, remote missing — should fall back to normal handshake
    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let client_config = ClientConfig {
        local_sctp_init: Some(local_init),
        ..Default::default()
    };
    let (_ch, assoc) = endpoint
        .connect(client_config, remote_addr)
        .expect("partial SNAP config should fall back to handshake");
    assert_eq!(
        assoc.state(),
        AssociationState::CookieWait,
        "should use normal handshake when only local init provided"
    );

    // Only remote_sctp_init set, local missing — should fall back to normal handshake
    let remote_init = generate_snap_token(&TransportConfig::default()).expect("remote init");
    let client_config = ClientConfig {
        remote_sctp_init: Some(remote_init),
        ..Default::default()
    };
    let (_ch, assoc) = endpoint
        .connect(client_config, remote_addr)
        .expect("partial SNAP config should fall back to handshake");
    assert_eq!(
        assoc.state(),
        AssociationState::CookieWait,
        "should use normal handshake when only remote init provided"
    );
}

#[test]
fn test_snap_rejects_reused_local_init() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    // Generate a single local init — reusing it causes a collision
    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");

    let remote1 = generate_snap_token(&TransportConfig::default()).expect("remote init 1");
    let remote2 = generate_snap_token(&TransportConfig::default()).expect("remote init 2");

    // First connect succeeds
    let cfg1 = ClientConfig::new().with_snap(local_init.clone(), remote1);
    let res1 = endpoint.connect(cfg1, remote_addr);
    assert!(res1.is_ok(), "first SNAP connect should succeed");

    // Second connect with same local_init collides on local_aid
    let cfg2 = ClientConfig::new().with_snap(local_init, remote2);
    let res2 = endpoint.connect(cfg2, remote_addr);
    assert!(
        matches!(
            res2,
            Err(ConnectError::Snap(SnapError::AidCollision { .. }))
        ),
        "second SNAP connect should fail due to AID collision"
    );
}

#[test]
fn test_snap_rejects_local_init_ack() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let remote_init = generate_snap_token(&TransportConfig::default()).expect("remote init");
    let local_init_ack = ChunkInit {
        is_ack: true,
        initiate_tag: 1,
        ..Default::default()
    };
    let local_bytes = local_init_ack.marshal().expect("marshal local INIT-ACK");

    let client_config = ClientConfig::new().with_snap(local_bytes, remote_init);
    let res = endpoint.connect(client_config, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::InvalidInitAck {
                side: SnapSide::Local
            }))
        ),
        "local INIT-ACK should be rejected"
    );
}

#[test]
fn test_snap_rejects_local_init_with_zero_initiate_tag() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let remote_init = generate_snap_token(&TransportConfig::default()).expect("remote init");
    let local_init = ChunkInit {
        is_ack: false,
        initiate_tag: 0,
        ..Default::default()
    };
    let local_bytes = local_init.marshal().expect("marshal local INIT");

    let client_config = ClientConfig::new().with_snap(local_bytes, remote_init);
    let res = endpoint.connect(client_config, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ZeroInitiateTag {
                side: SnapSide::Local
            }))
        ),
        "local INIT with initiate_tag=0 should be rejected"
    );
}

// ---------------------------------------------------------------------------
// SNAP hardening tests: size limits, truncated / empty / oversized input,
// wrong chunk types, RFC field validation, round-trip token consistency.
// ---------------------------------------------------------------------------

#[test]
fn test_snap_rejects_empty_bytes() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let valid = generate_snap_token(&TransportConfig::default()).expect("valid init");

    // Empty remote
    let cfg = ClientConfig::new().with_snap(valid.clone(), Bytes::new());
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "empty remote bytes should fail parsing"
    );

    // Empty local
    let cfg = ClientConfig::new().with_snap(Bytes::new(), valid);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Local,
                ..
            }))
        ),
        "empty local bytes should fail parsing"
    );
}

#[test]
fn test_snap_rejects_truncated_bytes() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let valid = generate_snap_token(&TransportConfig::default()).expect("valid init");

    // Truncate to just 10 bytes — not enough for INIT header + fixed fields
    let truncated = valid.slice(..10.min(valid.len()));

    let cfg = ClientConfig::new().with_snap(valid.clone(), truncated.clone());
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "truncated remote bytes should fail parsing"
    );

    let cfg = ClientConfig::new().with_snap(truncated, valid);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Local,
                ..
            }))
        ),
        "truncated local bytes should fail parsing"
    );
}

#[test]
fn test_snap_rejects_oversized_bytes() {
    use crate::config::MAX_SNAP_INIT_BYTES;

    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let valid = generate_snap_token(&TransportConfig::default()).expect("valid init");
    let oversized = Bytes::from(vec![0u8; MAX_SNAP_INIT_BYTES + 1]);

    // Oversized remote
    let cfg = ClientConfig::new().with_snap(valid.clone(), oversized.clone());
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::OversizedInit {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "oversized remote bytes should be rejected"
    );

    // Oversized local
    let cfg = ClientConfig::new().with_snap(oversized, valid);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::OversizedInit {
                side: SnapSide::Local,
                ..
            }))
        ),
        "oversized local bytes should be rejected"
    );
}

#[test]
fn test_snap_rejects_remote_init_zero_inbound_streams() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let bad_remote = ChunkInit {
        is_ack: false,
        initiate_tag: 0xDEAD_BEEF,
        initial_tsn: 1,
        num_outbound_streams: 1,
        num_inbound_streams: 0, // RFC violation
        advertised_receiver_window_credit: 65535,
        ..Default::default()
    };
    let remote_bytes = bad_remote.marshal().expect("marshal bad remote INIT");

    let cfg = ClientConfig::new().with_snap(local_init, remote_bytes);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::InvalidInit {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "remote INIT with num_inbound_streams=0 should be rejected: got {:?}",
        res
    );
}

#[test]
fn test_snap_rejects_remote_init_zero_outbound_streams() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let bad_remote = ChunkInit {
        is_ack: false,
        initiate_tag: 0xDEAD_BEEF,
        initial_tsn: 1,
        num_outbound_streams: 0, // RFC violation
        num_inbound_streams: 1,
        advertised_receiver_window_credit: 65535,
        ..Default::default()
    };
    let remote_bytes = bad_remote.marshal().expect("marshal bad remote INIT");

    let cfg = ClientConfig::new().with_snap(local_init, remote_bytes);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::InvalidInit {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "remote INIT with num_outbound_streams=0 should be rejected: got {:?}",
        res
    );
}

#[test]
fn test_snap_rejects_remote_init_small_arwnd() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let bad_remote = ChunkInit {
        is_ack: false,
        initiate_tag: 0xDEAD_BEEF,
        initial_tsn: 1,
        num_outbound_streams: 1,
        num_inbound_streams: 1,
        advertised_receiver_window_credit: 100, // RFC says >= 1500
        ..Default::default()
    };
    let remote_bytes = bad_remote.marshal().expect("marshal bad remote INIT");

    let cfg = ClientConfig::new().with_snap(local_init, remote_bytes);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::InvalidInit {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "remote INIT with a_rwnd < 1500 should be rejected: got {:?}",
        res
    );
}

#[test]
fn test_snap_rejects_local_init_zero_inbound_streams() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let remote_init = generate_snap_token(&TransportConfig::default()).expect("remote init");
    let bad_local = ChunkInit {
        is_ack: false,
        initiate_tag: 0xCAFE_BABE,
        initial_tsn: 1,
        num_outbound_streams: 1,
        num_inbound_streams: 0,
        advertised_receiver_window_credit: 65535,
        ..Default::default()
    };
    let local_bytes = bad_local.marshal().expect("marshal bad local INIT");

    let cfg = ClientConfig::new().with_snap(local_bytes, remote_init);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::InvalidInit {
                side: SnapSide::Local,
                ..
            }))
        ),
        "local INIT with num_inbound_streams=0 should be rejected: got {:?}",
        res
    );
}

#[test]
fn test_snap_rejects_local_init_small_arwnd() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let remote_init = generate_snap_token(&TransportConfig::default()).expect("remote init");
    let bad_local = ChunkInit {
        is_ack: false,
        initiate_tag: 0xCAFE_BABE,
        initial_tsn: 1,
        num_outbound_streams: 1,
        num_inbound_streams: 1,
        advertised_receiver_window_credit: 0,
        ..Default::default()
    };
    let local_bytes = bad_local.marshal().expect("marshal bad local INIT");

    let cfg = ClientConfig::new().with_snap(local_bytes, remote_init);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::InvalidInit {
                side: SnapSide::Local,
                ..
            }))
        ),
        "local INIT with a_rwnd=0 should be rejected: got {:?}",
        res
    );
}

#[test]
fn test_snap_generate_token_roundtrip_is_valid() {
    let config = TransportConfig::default();
    let bytes = generate_snap_token(&config).expect("generate token");

    assert!(
        bytes.len() <= crate::config::MAX_SNAP_INIT_BYTES,
        "generated token should be within MAX_SNAP_INIT_BYTES"
    );

    let init = ChunkInit::unmarshal(&bytes).expect("unmarshal generated token");
    assert!(!init.is_ack, "generated token should be INIT, not INIT-ACK");
    assert_ne!(init.initiate_tag, 0, "initiate_tag must not be zero");
    assert_ne!(init.initial_tsn, 0, "initial_tsn must not be zero");
    assert_eq!(init.num_outbound_streams, u16::MAX);
    assert_eq!(init.num_inbound_streams, u16::MAX);
    assert_eq!(
        init.advertised_receiver_window_credit,
        config.max_receive_buffer_size()
    );

    // check() should pass for a token we generated ourselves
    init.check()
        .expect("check() should pass on generated token");
}

#[test]
fn test_snap_generate_token_unique_per_call() {
    let config = TransportConfig::default();
    let t1 = generate_snap_token(&config).expect("token 1");
    let t2 = generate_snap_token(&config).expect("token 2");
    assert_ne!(t1, t2, "two generated tokens should differ (random values)");

    let i1 = ChunkInit::unmarshal(&t1).expect("parse t1");
    let i2 = ChunkInit::unmarshal(&t2).expect("parse t2");
    assert_ne!(
        i1.initiate_tag, i2.initiate_tag,
        "initiate_tags should differ"
    );
}

#[test]
fn test_snap_rejects_wrong_chunk_type_bytes() {
    use crate::chunk::Chunk;
    use crate::chunk::chunk_selective_ack::ChunkSelectiveAck;

    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let valid = generate_snap_token(&TransportConfig::default()).expect("valid init");

    // Build a SACK chunk and try to use it as a SNAP INIT
    let sack = ChunkSelectiveAck {
        cumulative_tsn_ack: 1,
        advertised_receiver_window_credit: 65535,
        gap_ack_blocks: vec![],
        duplicate_tsn: vec![],
    };
    let sack_bytes = sack.marshal().expect("marshal SACK");

    let cfg = ClientConfig::new().with_snap(valid, sack_bytes);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "SACK bytes should fail to parse as INIT: got {:?}",
        res
    );
}

#[test]
fn test_snap_rejects_both_sides_empty() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let cfg = ClientConfig::new().with_snap(Bytes::new(), Bytes::new());
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(res, Err(ConnectError::Snap(SnapError::ParseFailed { .. }))),
        "two empty byte slices should fail"
    );
}

#[test]
fn test_snap_rejects_random_garbage_bytes() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let valid = generate_snap_token(&TransportConfig::default()).expect("valid init");

    // 50 bytes of pseudo-random garbage
    let garbage = Bytes::from(vec![0xAB; 50]);

    let cfg = ClientConfig::new().with_snap(valid.clone(), garbage.clone());
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Remote,
                ..
            }))
        ),
        "garbage remote bytes should fail: got {:?}",
        res
    );

    let cfg = ClientConfig::new().with_snap(garbage, valid);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(
            res,
            Err(ConnectError::Snap(SnapError::ParseFailed {
                side: SnapSide::Local,
                ..
            }))
        ),
        "garbage local bytes should fail: got {:?}",
        res
    );
}

#[test]
fn test_snap_identical_local_and_remote_init() {
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let token = generate_snap_token(&TransportConfig::default()).expect("token");

    let cfg = ClientConfig::new().with_snap(token.clone(), token);
    let res = endpoint.connect(cfg, remote_addr);
    assert!(
        matches!(res, Err(ConnectError::Snap(SnapError::AidCollision { .. }))),
        "same token for both sides should collide: got {:?}",
        res
    );
}

#[test]
fn test_snap_end_to_end_bidirectional_data() {
    // Two endpoints, both using Endpoint::connect with SNAP, exchanging data
    // through the full Endpoint::handle pipeline.
    let endpoint_config = Arc::new(EndpointConfig::default());
    let transport = TransportConfig::default();

    let init_a = generate_snap_token(&transport).expect("init A");
    let init_b = generate_snap_token(&transport).expect("init B");

    let addr_a: SocketAddr = SocketAddr::new(
        Ipv6Addr::LOCALHOST.into(),
        CLIENT_PORTS.lock().unwrap().next().unwrap(),
    );
    let addr_b: SocketAddr = SocketAddr::new(
        Ipv6Addr::LOCALHOST.into(),
        CLIENT_PORTS.lock().unwrap().next().unwrap(),
    );

    // Both sides are clients (no server config) — this is the SNAP model.
    let mut ep_a = Endpoint::new(endpoint_config.clone(), None);
    let mut ep_b = Endpoint::new(endpoint_config, None);

    let cfg_a = ClientConfig::new().with_snap(init_a.clone(), init_b.clone());
    let (ch_a, mut assoc_a) = ep_a.connect(cfg_a, addr_b).expect("SNAP connect A");

    let cfg_b = ClientConfig::new().with_snap(init_b, init_a);
    let (ch_b, mut assoc_b) = ep_b.connect(cfg_b, addr_a).expect("SNAP connect B");

    // Both should be established immediately.
    assert_eq!(assoc_a.state(), AssociationState::Established);
    assert_eq!(assoc_b.state(), AssociationState::Established);
    assert_matches!(assoc_a.poll(), Some(Event::Connected));
    assert_matches!(assoc_b.poll(), Some(Event::Connected));

    let now = Instant::now();

    // A writes to B.
    let si = 1u16;
    let msg_a = Bytes::from_static(b"hello from A");
    let mut stream_a = assoc_a
        .open_stream(si, PayloadProtocolIdentifier::Binary)
        .expect("open stream A");
    stream_a
        .write_sctp(&msg_a, PayloadProtocolIdentifier::Binary)
        .expect("write A");

    // Pump transmits from A into B via Endpoint::handle.
    while let Some(transmit) = assoc_a.poll_transmit(now) {
        if let Payload::RawEncode(datagrams) = transmit.payload {
            for dgram in datagrams {
                if let Some((handle, event)) = ep_b.handle(now, addr_a, None, None, dgram) {
                    assert_eq!(handle, ch_b);
                    if let DatagramEvent::AssociationEvent(ev) = event {
                        assoc_b.handle_event(ev);
                    }
                }
            }
        }
    }

    // B should now have data.
    let accepted = assoc_b.accept_stream().expect("accept stream on B");
    assert_eq!(accepted.stream_identifier, si);

    let mut buf = vec![0u8; 64];
    let chunks = assoc_b
        .stream(si)
        .expect("get stream B")
        .read_sctp()
        .expect("read B")
        .expect("expected data");
    let n = chunks.read(&mut buf).expect("read payload");
    assert_eq!(&buf[..n], b"hello from A");

    // B writes back to A.
    let msg_b = Bytes::from_static(b"hello from B");
    assoc_b
        .stream(si)
        .expect("get stream B for write")
        .write_sctp(&msg_b, PayloadProtocolIdentifier::Binary)
        .expect("write B");

    // Pump transmits from B into A.
    while let Some(transmit) = assoc_b.poll_transmit(now) {
        if let Payload::RawEncode(datagrams) = transmit.payload {
            for dgram in datagrams {
                if let Some((handle, event)) = ep_a.handle(now, addr_b, None, None, dgram) {
                    assert_eq!(handle, ch_a);
                    if let DatagramEvent::AssociationEvent(ev) = event {
                        assoc_a.handle_event(ev);
                    }
                }
            }
        }
    }

    // A should now have data back from B.
    let chunks = assoc_a
        .stream(si)
        .expect("get stream A")
        .read_sctp()
        .expect("read A")
        .expect("expected data from B");
    let n = chunks.read(&mut buf).expect("read payload from B");
    assert_eq!(&buf[..n], b"hello from B");
}

#[test]
fn test_snap_association_drains_cleanly() {
    // Verify that a SNAP association, once drained, properly cleans up both
    // association_ids and association_ids_init in the endpoint.
    let mut endpoint = Endpoint::new(Arc::new(EndpointConfig::default()), None);
    let remote_addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

    let local_init = generate_snap_token(&TransportConfig::default()).expect("local init");
    let remote_init = generate_snap_token(&TransportConfig::default()).expect("remote init");

    let local_tag = ChunkInit::unmarshal(&local_init)
        .expect("parse local")
        .initiate_tag;
    let remote_tag = ChunkInit::unmarshal(&remote_init)
        .expect("parse remote")
        .initiate_tag;

    let cfg = ClientConfig::new().with_snap(local_init, remote_init);
    let (ch, _assoc) = endpoint.connect(cfg, remote_addr).expect("SNAP connect");

    // Endpoint should have entries in both routing tables.
    assert!(
        endpoint.association_ids.contains_key(&local_tag),
        "local_tag should be in association_ids"
    );
    assert!(
        endpoint.association_ids_init.contains_key(&remote_tag),
        "remote_tag should be in association_ids_init"
    );

    // Simulate draining: send the Drained endpoint event.
    let drain_event = EndpointEvent(EndpointEventInner::Drained);
    endpoint.handle_event(ch, drain_event);

    // Both routing tables should be cleaned up.
    assert!(
        !endpoint.association_ids.contains_key(&local_tag),
        "local_tag should be removed from association_ids after drain"
    );
    assert!(
        !endpoint.association_ids_init.contains_key(&remote_tag),
        "remote_tag should be removed from association_ids_init after drain"
    );
}

#[test]
#[ignore = "manual end-to-end transfer measurement"]
fn measure_queue_transfer() {
    for (name, transfer) in [
        (
            "unfragmented",
            test_assoc_reliable_simple as fn() -> Result<()>,
        ),
        (
            "fragmented",
            test_assoc_reliable_ordered_fragmented_then_defragmented,
        ),
    ] {
        let mut samples = Vec::new();
        for _ in 0..21 {
            let start = Instant::now();
            transfer().unwrap();
            samples.push(start.elapsed().as_nanos());
        }
        samples.sort_unstable();
        std::println!(
            "{name}: median={} ns min={} max={}",
            samples[10],
            samples[0],
            samples[20]
        );
    }
}
