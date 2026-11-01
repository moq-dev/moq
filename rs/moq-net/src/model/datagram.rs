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
//! A datagram is never cached or served by a fetch. Only the short per-track send buffer holds
//! it, which a new subscription may still read. That buffer bills what it retains to the
//! track's [`Pool`](crate::cache::Pool), like cached groups, so pressure drops the oldest.
//!
//! Wire counterparts: [`crate::lite::Datagram`] and moq-transport's
//! [`crate::ietf::ObjectDatagram`], whose Group ID is the sequence. Relays preserve its Object
//! IDs and statuses between moq-transport peers and across a moq-lite hop negotiating `ietf`.
//! The plain reader and a lite hop without that extension deliver only normal object 0
//! payloads, dropping other objects and statuses without refusing the track.

use bytes::Bytes;

use crate::Timestamp;
use crate::cache;
use std::sync::Mutex;

/// Hard ceiling on a datagram payload, matching the QUIC DATAGRAM frame limit.
///
/// This only bounds buffering; the real limit is per hop. Each session drops a datagram whose
/// encoded body exceeds the transport's current datagram size (roughly the path MTU minus QUIC
/// and MoQ header overhead), so callers should keep payloads well below the minimum path MTU
/// of 1200 bytes (e.g. a single audio frame).
pub(super) const MAX_DATAGRAM_PAYLOAD: usize = u16::MAX as usize;

/// A single unreliable payload on a track: a sequence number, an optional presentation timestamp, and the bytes.
///
/// The sequence number is drawn from the same namespace as the track's groups, so a relay can forward
/// a datagram while preserving the origin's numbering (see [`super::track::Producer::insert_datagram`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// Per-track sequence number, shared with the group namespace.
	pub sequence: u64,
	/// Presentation timestamp in the track's timescale, or `None` when untimed.
	pub timestamp: Option<Timestamp>,
	/// The datagram payload.
	pub payload: Bytes,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Wire {
	pub object_id: Option<u64>,
	pub priority: Option<u8>,
	pub end_of_group: bool,
	pub status: Option<u64>,
	pub properties: super::object::Properties,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stored {
	pub datagram: Datagram,
	pub wire: Option<std::sync::Arc<Wire>>,
	/// On a compressed track, the payload as its own DEFLATE stream: a datagram is a
	/// one-frame group, so it compresses with a fresh window. Kept beside the inflated
	/// `datagram.payload`, since both are small and short-lived.
	pub deflated: Option<Bytes>,
}

impl Stored {
	/// The payload a session sends, with its inflated size when that is the DEFLATE slice:
	/// only a session that carries `compression` takes a compressed track's stored form.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub fn sent(&self, compression: bool) -> (&Bytes, Option<u64>) {
		match &self.deflated {
			Some(deflated) if compression => (deflated, Some(self.datagram.payload.len() as u64)),
			_ => (&self.datagram.payload, None),
		}
	}

	/// The bytes the track stores and meters: the DEFLATE slice when it has one.
	pub fn stored_len(&self) -> u64 {
		self.deflated.as_ref().unwrap_or(&self.datagram.payload).len() as u64
	}

	/// What buffering it costs the pool: both payload forms, any wire metadata, and its slot.
	fn cache_size(&self) -> u64 {
		let wire = self.wire.as_ref().map_or(0, |wire| {
			size_of::<Wire>() + wire.properties.block.as_ref().map_or(0, |block| block.bytes.len())
		});
		let deflated = self.deflated.as_ref().map_or(0, Bytes::len);
		(self.datagram.payload.len() + deflated + wire + SLOT_OVERHEAD) as u64
	}
}

/// Heap a buffered datagram's slot takes, doubled because a `VecDeque` holds up to twice
/// its entries.
const SLOT_OVERHEAD: usize = 2 * size_of::<Buffered>();

/// A datagram in a track's send buffer, billed to the track's pool until it leaves.
///
/// Readers take clones of [`Self::stored`], which alias its payloads rather than copy them,
/// so only the buffer is billed.
pub(super) struct Buffered {
	pub sequence: u64,
	pub timestamp: Option<Timestamp>,
	// Released in place once an ended track's datagram expires: the closed track state
	// refuses the write that would pop it, and a stale reader would otherwise pin it.
	held: Mutex<Option<(Stored, cache::Bill)>>,
}

impl Buffered {
	/// Bill `stored` to `cache`, accruing eviction debt like a group's frames.
	pub fn new(stored: Stored, cache: &std::sync::Arc<cache::Track>) -> Self {
		let bill = cache.bill(stored.cache_size());
		Self {
			sequence: stored.sequence,
			timestamp: stored.timestamp,
			held: Mutex::new(Some((stored, bill))),
		}
	}

	/// A reader's alias of it, `None` once released.
	pub fn stored(&self) -> Option<Stored> {
		self.held.lock().unwrap().as_ref().map(|(stored, _)| stored.clone())
	}

	/// What it bills the pool, zero once released.
	pub fn bytes(&self) -> u64 {
		self.held.lock().unwrap().as_ref().map_or(0, |(_, bill)| bill.bytes())
	}

	/// Whether nobody has written it for the pool's idle window, or it was released.
	pub fn expired(&self) -> bool {
		self.held
			.lock()
			.unwrap()
			.as_ref()
			.is_none_or(|(_, bill)| bill.expired())
	}

	/// Release its payloads and bill once expired, for an ended track that cannot pop it.
	/// Returns whether it is released.
	pub fn release_expired(&self) -> bool {
		let mut held = self.held.lock().unwrap();
		if held.as_ref().is_some_and(|(_, bill)| !bill.expired()) {
			return false;
		}
		*held = None;
		true
	}
}

impl From<Datagram> for Stored {
	fn from(datagram: Datagram) -> Self {
		Self {
			datagram,
			wire: None,
			deflated: None,
		}
	}
}

impl std::ops::Deref for Stored {
	type Target = Datagram;
	fn deref(&self) -> &Datagram {
		&self.datagram
	}
}
