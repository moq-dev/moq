//! QUIC transport protocol implementation
//!
//! [QUIC](https://en.wikipedia.org/wiki/QUIC) is a modern transport protocol addressing
//! shortcomings of TCP, such as head-of-line blocking, poor security, slow handshakes, and
//! inefficient congestion control. This crate provides a portable userspace implementation. It
//! builds on top of quinn-proto, which implements protocol logic independent of any particular
//! runtime.
//!
//! The entry point of this crate is the [`Endpoint`].
//!
//! # About QUIC
//!
//! A QUIC connection is an association between two endpoints. The endpoint which initiates the
//! connection is termed the client, and the endpoint which accepts it is termed the server. A
//! single endpoint may function as both client and server for different connections, for example
//! in a peer-to-peer application. To communicate application data, each endpoint may open streams
//! up to a limit dictated by its peer. Typically, that limit is increased as old streams are
//! finished.
//!
//! Streams may be unidirectional or bidirectional, and are cheap to create and disposable. For
//! example, a traditionally datagram-oriented application could use a new stream for every
//! message it wants to send, no longer needing to worry about MTUs. Bidirectional streams behave
//! much like a traditional TCP connection, and are useful for sending messages that have an
//! immediate response, such as an HTTP request. Stream data is delivered reliably, and there is no
//! ordering enforced between data on different streams.
//!
//! By avoiding head-of-line blocking and providing unified congestion control across all streams
//! of a connection, QUIC is able to provide higher throughput and lower latency than one or
//! multiple TCP connections between the same two hosts, while providing more useful behavior than
//! raw UDP sockets.
//!
//! Quinn also exposes unreliable datagrams, which are a low-level primitive preferred when
//! automatic fragmentation and retransmission of certain data is not desired.
//!
//! QUIC uses encryption and identity verification built directly on TLS 1.3. Just as with a TLS
//! server, it is useful for a QUIC server to be identified by a certificate signed by a trusted
//! authority. If this is infeasible--for example, if servers are short-lived or not associated
//! with a domain name--then as with TLS, self-signed certificates can be used to provide
//! encryption alone.
#![warn(missing_docs)]

use std::pin::Pin;

mod connection;
mod endpoint;
mod incoming;
mod mutex;
mod recv_stream;
mod runtime;
mod send_stream;
mod work_limiter;

pub(crate) use moq_clock::Instant;
pub(crate) use std::time::Duration;

pub use moq_quic::BloomTokenLog;
pub use moq_quic::{
    AckFrequencyConfig, ApplicationClose, Chunk, ClientConfig, ClosedStream, ConfigError,
    ConnectError, ConnectionClose, ConnectionError, ConnectionId, ConnectionIdGenerator,
    ConnectionStats, Dir, EcnCodepoint, EndpointConfig, FrameStats, FrameType, IdleTimeout,
    InvalidCid, MtuDiscoveryConfig, NoneTokenLog, NoneTokenStore, PathStats, ServerConfig, Side,
    StdSystemTime, StreamId, TimeSource, TokenLog, TokenMemoryCache, TokenReuseError, TokenStore,
    Transmit, TransportConfig, TransportErrorCode, UdpStats, ValidationTokenConfig, VarInt,
    VarIntBoundsExceeded, Written, congestion, crypto,
};
#[cfg(feature = "qlog")]
pub use moq_quic::{QlogConfig, QlogStream};

pub use crate::quic::driver::connection::{
    AcceptBi, AcceptUni, Connecting, Connection, OpenBi, OpenUni, ReadDatagram,
    RemoteAddressWatcher, SendDatagram, SendDatagramError,
};
pub use crate::quic::driver::endpoint::{Accept, Endpoint, EndpointStats};
pub use crate::quic::driver::incoming::{Incoming, IncomingFuture, RetryError};
pub use crate::quic::driver::recv_stream::{
    Credit, ReadError, ReadExactError, ReadToEndError, RecvStream, ResetError,
};
#[cfg(test)]
pub(crate) use crate::quic::driver::runtime::wrap_udp_socket;
pub use crate::quic::driver::runtime::{AsyncUdpSocket, UdpSender};
pub use crate::quic::driver::send_stream::{Checkpoint, SendStream, StoppedError, WriteError};

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub(crate) enum ConnectionEvent {
    Close {
        error_code: VarInt,
        reason: bytes::Bytes,
    },
    Proto(moq_quic::ConnectionEvent),
    Rebind(Pin<Box<dyn UdpSender>>),
}

fn udp_transmit<'a>(t: &Transmit, buffer: &'a [u8]) -> moq_sock::udp::Transmit<'a> {
    moq_sock::udp::Transmit {
        destination: t.destination,
        ecn: t.ecn.map(udp_ecn),
        contents: buffer,
        segment_size: t.segment_size,
        src_ip: t.src_ip,
    }
}

fn udp_ecn(ecn: EcnCodepoint) -> moq_sock::udp::EcnCodepoint {
    match ecn {
        EcnCodepoint::Ect0 => moq_sock::udp::EcnCodepoint::Ect0,
        EcnCodepoint::Ect1 => moq_sock::udp::EcnCodepoint::Ect1,
        EcnCodepoint::Ce => moq_sock::udp::EcnCodepoint::Ce,
    }
}

/// Maximum number of datagrams processed in send/recv calls to make before moving on to other processing
///
/// This helps ensure we don't starve anything when the CPU is slower than the link.
/// Value is selected by picking a low number which didn't degrade throughput in benchmarks.
const IO_LOOP_BOUND: usize = 160;

/// The maximum amount of time that should be spent in `recvmsg()` calls per endpoint iteration
///
/// 50us are chosen so that an endpoint iteration with a 50us sendmsg limit blocks
/// the runtime for a maximum of about 100us.
/// Going much lower does not yield any noticeable difference, since a single `recvmmsg`
/// batch of size 32 was observed to take 30us on some systems.
const RECV_TIME_BOUND: Duration = Duration::from_micros(50);
