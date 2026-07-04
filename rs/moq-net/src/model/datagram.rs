//! A datagram is a single unreliable payload delivered on a track, parallel to groups.
//!
//! Unlike a group (an ordered stream of frames over a QUIC stream), a datagram is one self-contained
//! payload carried in a single QUIC datagram: best-effort, unordered, and never retransmitted. It
//! shares the track's monotonic sequence-number namespace with groups but is otherwise independent,
//! produced via [`super::track::Producer::append_datagram`] / [`super::track::Producer::write_datagram`]
//! and consumed via [`super::track::Subscriber::recv_datagram`].
//!
//! Wire counterpart: [`crate::lite::Datagram`].

use bytes::Bytes;

use crate::Timestamp;

/// Maximum datagram payload size, in bytes.
///
/// A datagram body (sequence + timestamp + payload varints, plus this payload) must fit in a single
/// QUIC datagram without IP fragmentation, so the payload is capped conservatively below the minimum
/// path MTU. Producers reject a larger payload with [`crate::Error::WrongSize`]; there is no group
/// fallback, so callers keep datagram payloads small (e.g. a single audio frame).
pub const MAX_DATAGRAM_PAYLOAD: usize = 1200;

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
