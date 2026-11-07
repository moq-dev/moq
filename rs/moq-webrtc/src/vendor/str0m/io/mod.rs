#![allow(clippy::manual_range_contains)]
#![allow(clippy::new_without_default)]

use std::convert::TryFrom;
use std::fmt;
use std::io;
use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

pub use is::stun::StunMessage;

pub use str0m_proto::DATAGRAM_MTU_TARGET;
pub use str0m_proto::DATAGRAM_MTU_TARGET_MAX;
pub use str0m_proto::DATAGRAM_MTU_TARGET_MIN;
pub use str0m_proto::DATAGRAM_MTU_WARN;

/// Max UDP packet size
pub(crate) const DATAGRAM_MAX_PACKET_SIZE: usize = 2000;

/// Max expected RTP header over, with full extensions etc.
pub const MAX_RTP_OVERHEAD: usize = 80;

/// Max expected DTLS record overhead (record header + explicit nonce + AEAD
/// auth tag), worst case across DTLS 1.2/1.3 and common AEAD ciphers
/// (AES-GCM, AES-CCM, CHACHA20-POLY1305).
pub const MAX_DTLS_OVERHEAD: usize = 48;

mod error;
pub use self::error::NetError;
#[allow(unused_imports)]
pub use self::error::StunError;

pub use str0m_proto::{DatagramSend, Protocol, TcpType, Transmit};

#[derive(Debug, Serialize, Deserialize)]
/// Received incoming data.
pub struct Receive<'a> {
    /// The protocol the socket this received data originated from is using.
    pub proto: Protocol,

    /// The socket this received data originated from.
    pub source: SocketAddr,

    /// The destination ip of the datagram.
    pub destination: SocketAddr,

    /// Parsed contents of the datagram.
    #[serde(borrow)]
    pub contents: DatagramRecv<'a>,
}

impl<'a> Receive<'a> {
    /// Creates a new instance by trying to parse the contents of `buf`.
    pub fn new(
        proto: Protocol,
        source: SocketAddr,
        destination: SocketAddr,
        buf: &'a [u8],
    ) -> Result<Self, NetError> {
        let contents = DatagramRecv::try_from(buf)?;
        Ok(Receive {
            proto,
            source,
            destination,
            contents,
        })
    }
}

/// Wrapper for a parsed payload to be received.
#[derive(Serialize, Deserialize)]
pub struct DatagramRecv<'a> {
    #[serde(borrow)]
    pub(crate) inner: DatagramRecvInner<'a>,
}

#[allow(clippy::large_enum_variant)] // We purposely don't want to allocate.
#[derive(Serialize, Deserialize)]
pub(crate) enum DatagramRecvInner<'a> {
    Stun(StunMessage<'a>),
    Dtls(&'a [u8]),
    Rtp(&'a [u8]),
    Rtcp(&'a [u8]),
}

impl<'a> TryFrom<&'a [u8]> for DatagramRecv<'a> {
    type Error = NetError;

    fn try_from(value: &'a [u8]) -> Result<Self, Self::Error> {
        use DatagramRecvInner::*;

        let kind = MultiplexKind::try_from(value)?;

        let inner = match kind {
            MultiplexKind::Stun => Stun(StunMessage::parse(value)?),
            MultiplexKind::Dtls => Dtls(value),
            MultiplexKind::Rtp => Rtp(value),
            MultiplexKind::Rtcp => Rtcp(value),
        };

        Ok(DatagramRecv { inner })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum MultiplexKind {
    Stun,
    Dtls,
    Rtp,
    Rtcp,
}

impl<'a> TryFrom<&'a [u8]> for MultiplexKind {
    type Error = io::Error;

    fn try_from(value: &'a [u8]) -> Result<Self, io::Error> {
        if value.is_empty() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Empty datagram"));
        }

        let byte0 = value[0];
        let len = value.len();

        if byte0 < 2 && len >= 20 {
            Ok(MultiplexKind::Stun)
        } else if byte0 >= 20 && byte0 < 64 {
            Ok(MultiplexKind::Dtls)
        } else if byte0 >= 128 && byte0 < 192 && len > 2 {
            let byte1 = value[1];
            let payload_type = byte1 & 0x7f;

            Ok(if payload_type < 64 {
                // This is kinda novel, and probably breaks, but...
                // we can use the < 64 pt as an escape hatch if we run out
                // of dynamic numbers >= 96
                // https://bugs.chromium.org/p/webrtc/issues/detail?id=12194
                MultiplexKind::Rtp
            } else if payload_type >= 64 && payload_type < 96 {
                MultiplexKind::Rtcp
            } else {
                MultiplexKind::Rtp
            })
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Unknown datagram",
            ))
        }
    }
}

impl<'a> TryFrom<&'a Transmit> for Receive<'a> {
    type Error = NetError;

    fn try_from(t: &'a Transmit) -> Result<Self, Self::Error> {
        Ok(Receive {
            proto: t.proto,
            source: t.source,
            destination: t.destination,
            contents: DatagramRecv::try_from(&t.contents[..])?,
        })
    }
}

impl fmt::Debug for DatagramRecv<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl fmt::Debug for DatagramRecvInner<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stun(v) => f.debug_tuple("Stun").field(v).finish(),
            Self::Dtls(v) => write!(f, "Dtls(len: {})", v.len()),
            Self::Rtp(v) => write!(f, "Rtp(len: {})", v.len()),
            Self::Rtcp(v) => write!(f, "Rtcp(len: {})", v.len()),
        }
    }
    //
}
