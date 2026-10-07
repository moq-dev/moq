//! A datagram is a single unreliable payload delivered on a track, parallel to groups.
//!
//! Unlike a group (an ordered stream of frames over a QUIC stream), a datagram is one self-contained
//! payload carried in a single QUIC datagram: best-effort, unordered, and never retransmitted. It
//! shares the track's monotonic sequence-number namespace with groups but is otherwise independent,
//! produced via [`super::track::Producer::append_datagram`] / [`super::track::Producer::insert_datagram`]
//! and consumed via [`super::track::Subscriber::recv_datagram`].
//!
//! Delivery is best-effort per hop: a session drops (with a debug log) any datagram whose encoded
//! body exceeds the transport's datagram size, and sessions that can't carry datagrams at all
//! (moq-lite before 05, or stream-only transports like WebSocket) never deliver them.
//!
//! A datagram is live-only: it reaches the subscriptions open when it is pushed, and is never
//! cached, replayed to a later subscription, or served by a fetch.
//!
//! Wire counterparts: [`crate::lite::Datagram`], and on moq-transport an OBJECT_DATAGRAM at
//! object 0 whose Group ID is the sequence ([`crate::ietf::ObjectDatagram`]).

use std::sync::atomic::{AtomicU64, Ordering};

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
/// a datagram while preserving the origin's numbering (see [`super::track::Producer::insert_datagram`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// Per-track sequence number, shared with the group namespace.
	pub sequence: u64,
	/// Presentation timestamp in the track's timescale.
	pub timestamp: Timestamp,
	/// The datagram payload.
	pub payload: Bytes,
}

/// A point on the process-wide datagram clock, which orders every datagram push against every
/// subscription opening. A subscription takes the datagrams pushed after it opened, whichever
/// copy of the track (a relay's upstream, a replacement route) they reach it through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Tick(u64);

impl Tick {
	/// A tick later than every one taken before it, on any thread.
	pub(crate) fn next() -> Self {
		static CLOCK: AtomicU64 = AtomicU64::new(0);
		Self(CLOCK.fetch_add(1, Ordering::Relaxed))
	}
}
