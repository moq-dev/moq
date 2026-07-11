//! A datagram is a single unreliable payload delivered on a track, parallel to groups.
//!
//! Unlike a group (an ordered stream of frames over a QUIC stream), a datagram is one self-contained
//! payload carried in a single QUIC datagram: best-effort, unordered, and never retransmitted. It
//! shares the track's monotonic sequence-number namespace with groups but is otherwise independent,
//! produced via [`super::track::Producer::append_datagram`] / [`super::track::Producer::write_datagram`]
//! and consumed via [`super::track::Subscriber::recv_datagram`].
//!
//! Delivery is best-effort per hop: a session drops (with a debug log) any datagram whose encoded
//! body exceeds the transport's datagram size, and sessions that can't carry datagrams at all
//! (IETF moq-transport, moq-lite before 05, or stream-only transports like WebSocket) never
//! deliver them.
//!
//! Wire counterpart: [`crate::lite::Datagram`].

use bytes::Bytes;

use crate::Timestamp;

/// Hard ceiling on a datagram payload, matching the QUIC DATAGRAM frame limit.
///
/// This only bounds buffering; the real limit is per hop. Each session drops a datagram whose
/// encoded body exceeds the transport's current datagram size (roughly the path MTU minus QUIC
/// and MoQ header overhead), so callers should keep payloads well below the minimum path MTU
/// of 1200 bytes (e.g. a single audio frame).
pub(crate) const MAX_DATAGRAM_PAYLOAD: usize = u16::MAX as usize;

/// A single unreliable payload on a track: a sequence number, a presentation timestamp, and the bytes.
///
/// The sequence number is drawn from the same namespace as the track's groups, so a relay can forward
/// a datagram while preserving the origin's numbering (see [`super::track::Producer::write_datagram`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// Per-track sequence number, shared with the group namespace.
	pub sequence: u64,
	/// Presentation timestamp in the track's timescale.
	pub timestamp: Timestamp,
	/// The datagram payload.
	pub payload: Bytes,
}
