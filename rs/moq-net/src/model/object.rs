//! Private relay metadata; public readers see only the frame or datagram payload.
use bytes::Bytes;

use crate::Timestamp;

/// How the publisher serialized the keys inside immutable properties.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// private: passthrough
pub(crate) enum Encoding {
	Absolute,
	DeltaQuic,
	Delta { seven: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
// private: passthrough
pub(crate) enum Value {
	Int(u64),
	Bytes(Bytes),
}

#[derive(Clone, Debug, PartialEq, Eq)]
// private: passthrough
pub(crate) struct Property {
	pub id: u64,
	pub value: Value,
}

/// An object's relay properties, held as one encoded [`Block`].
///
/// On a timed track, the mutable Timestamp is never held: it is the frame's, which egress
/// restates. On an untimed one it has no units, so it stays in the block like any other.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
// private: passthrough
pub(crate) struct Properties {
	/// The Timestamp the block's Immutable Properties hold on a timed track, which keeps
	/// its ingress units.
	pub sealed_time: Option<Timestamp>,
	/// The properties, if there are any.
	pub block: Option<Block>,
}

/// An object's properties, but for a timed track's mutable Timestamp, as one encoded list.
///
/// Kept encoded because a decoded list costs many times its wire size, and a relay holds
/// one per cached object. Egress decodes it only to re-encode for another draft.
#[derive(Clone, Debug, PartialEq, Eq)]
// private: passthrough
pub(crate) struct Block {
	/// The properties in ascending ID order, serialized as `encoding` does. Never empty.
	pub bytes: Bytes,
	/// How `bytes` is serialized: its key and integer encoding.
	pub encoding: Encoding,
	/// `bytes` holds Immutable Properties (0x0b), serialized as `encoding` does.
	pub immutable: bool,
	/// Property 0x06 means SUBGROUP_DELIVERY_TIMEOUT on the ingress draft.
	pub delivery_timeout: bool,
}

impl Properties {
	#[cfg(feature = "async")]
	pub fn is_empty(&self) -> bool {
		self.block.is_none()
	}

	pub fn heap(&self) -> u64 {
		self.block.as_ref().map_or(0, |block| block.bytes.len() as u64)
	}
}

/// The Track Properties moq-net does not interpret, which a relay forwards unchanged.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
// private: passthrough
pub(crate) struct TrackProperties {
	/// In arrival order, with Immutable Properties (0x0b) as its serialized bytes.
	pub values: Vec<Property>,
	/// How the publisher serialized the keys inside Immutable Properties, when present.
	pub immutable: Option<Encoding>,
	/// Property 0x06 means SUBGROUP_DELIVERY_TIMEOUT on the ingress draft.
	pub delivery_timeout: bool,
}

/// Object numbering is separate from the public frame's ordinal in its group.
#[derive(Clone, Debug)]
// private: passthrough
pub(crate) struct Fields {
	pub id: u64,
	pub properties: Properties,
}

impl Fields {
	pub fn heap(&self) -> u64 {
		(size_of::<Self>() + 2 * size_of::<usize>()) as u64 + self.properties.heap()
	}
}
