//! Low-level protocol logic for the QUIC protocol
//!
//! moq-quic has no networking code: the caller supplies transport timestamps, and address
//! validation reads the ambient `kio::clock` wall time. Most applications use the Tokio driver
//! in `moq_tokio::quic::driver` instead.
//!
//! Use `moq_tokio::quic::driver` for Tokio, or drive this state machine from another runtime.
//!
//! The most important types are `Endpoint`, which conceptually represents the protocol state for
//! a single socket and mostly manages configuration and dispatches incoming datagrams to the
//! related `Connection`. `Connection` types contain the bulk of the protocol logic related to
//! managing a single connection and all the related state (such as streams).

#![warn(missing_docs)]
#![cfg_attr(test, allow(dead_code, reason = "test builds compile helpers only some tests call"))]

use std::{
	net::{IpAddr, SocketAddr},
	ops,
};

mod cid_queue;
mod coding;
mod constant_time;
mod entropy;
// Not part of the public API: compiled only for the crate's own tests, the benches, and
// the `fuzz/` harness. The connection target needs ring or aws-lc-rs for the endpoint's keys.
#[cfg(all(any(test, feature = "fuzz"), any(feature = "aws-lc-rs", feature = "ring")))]
#[doc(hidden)]
pub mod fuzz;
mod range_set;
#[cfg(all(test, any(feature = "aws-lc-rs", feature = "ring")))]
mod tests;
pub mod transport_parameters;
mod varint;

pub use varint::{VarInt, VarIntBoundsExceeded};

mod bloom_token_log;
use bloom_token_log::BloomTokenLog;

pub mod connection;
pub use crate::connection::{
	CreditStats, Datagrams, Event, FrameStats, PathStats, ShouldTransmit, StreamEvent, Streams, UdpStats, recv, send,
};

pub mod stream;
#[cfg(feature = "mux")]
#[doc(hidden)]
pub use connection::mux;
pub(crate) use stream::{Dir, Id as StreamId};

#[cfg(any(feature = "ring", feature = "aws-lc-rs"))]
pub use rustls;

mod config;
mod limits;
pub use config::{ConfigError, client, server, transport};
pub use limits::{Limits, LimitsError, Rate};

pub mod crypto;

mod frame;
use crate::frame::Frame;
pub use crate::frame::{ApplicationClose, ConnectionClose, Datagram, FrameType};

pub mod endpoint;

mod packet;
pub use packet::SpaceId;

mod shared;
pub use crate::shared::{ConnectionEvent, ConnectionId, EcnCodepoint, EndpointEvent};

mod transport_error;
pub use crate::transport_error::{Code as TransportErrorCode, Error as TransportError};

use moq_congestion as congestion;

mod cid_generator;
pub use crate::cid_generator::{ConnectionIdGenerator, InvalidCid, RandomConnectionIdGenerator};

mod token;
use token::ResetToken;
use token::{TokenLog, TokenReuseError, TokenStore};

mod token_memory_cache;
use token_memory_cache::TokenMemoryCache;

// Deal with time
// MoQ's clock, so a caller-driven clock (a test's or the simulator's) reaches the state machine.
pub(crate) use kio::clock::{Instant, Wall};
pub(crate) use std::time::Duration;
pub(crate) const UNIX_EPOCH: Wall = Wall::UNIX_EPOCH;

/// A QUIC version this crate speaks
#[derive(Debug, Copy, Clone, Default, Eq, PartialEq, Hash)]
#[non_exhaustive]
pub enum Version {
	/// QUIC version 1, [RFC 9000](https://www.rfc-editor.org/rfc/rfc9000.html)
	#[default]
	V1,
}

impl Version {
	/// Every version, in preference order
	const ALL: &[Self] = &[Self::V1];

	/// The version's number on the wire
	pub(crate) fn wire(self) -> u32 {
		match self {
			Self::V1 => 0x0000_0001,
		}
	}

	/// The version a wire number names, if this crate speaks it
	pub(crate) fn from_wire(version: u32) -> Option<Self> {
		Self::ALL.iter().copied().find(|v| v.wire() == version)
	}
}

/// Whether an endpoint was the initiator of a connection
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Side {
	/// The initiator of a connection
	Client = 0,
	/// The acceptor of a connection
	Server = 1,
}

impl Side {
	#[inline]
	/// Shorthand for `self == Side::Client`
	pub fn is_client(self) -> bool {
		self == Self::Client
	}

	#[inline]
	/// Shorthand for `self == Side::Server`
	pub fn is_server(self) -> bool {
		self == Self::Server
	}
}

impl ops::Not for Side {
	type Output = Self;
	fn not(self) -> Self {
		match self {
			Self::Client => Self::Server,
			Self::Server => Self::Client,
		}
	}
}

/// An outgoing packet
#[derive(Debug)]
#[must_use]
pub struct Transmit {
	/// The address to send the datagram to
	pub remote: SocketAddr,
	/// Explicit congestion notification bits to set on the packet
	pub ecn: Option<EcnCodepoint>,
	/// Amount of data written to the caller-supplied buffer
	pub size: usize,
	/// The segment size if this transmission contains multiple datagrams.
	/// This is `None` if the transmit only contains a single datagram
	pub segment_size: Option<usize>,
	/// The local IP address to send the datagram from, if the socket chooses one
	pub local_ip: Option<IpAddr>,
}

//
// Useful internal constants
//

/// The maximum number of CIDs we bother to issue per connection
const LOC_CID_COUNT: u64 = 8;
const RESET_TOKEN_SIZE: usize = 16;
const MAX_CID_SIZE: usize = 20;
const MIN_INITIAL_SIZE: u16 = 1200;
/// <https://www.rfc-editor.org/rfc/rfc9000.html#name-datagram-size>
const INITIAL_MTU: u16 = 1200;
const MAX_UDP_PAYLOAD: u16 = 65527;
const TIMER_GRANULARITY: Duration = Duration::from_millis(1);
/// Maximum number of streams that can be uniquely identified by a stream ID
const MAX_STREAM_COUNT: u64 = 1 << 60;
