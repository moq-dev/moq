//! Private relay metadata; public readers see only the frame or datagram payload.
use bytes::Bytes;

use crate::Timescale;

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

/// An object's relay properties. The common Timestamp is a field; every other property
/// stays in its encoded [`Block`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
// private: passthrough
pub(crate) struct Properties {
	pub timestamp: Option<u64>,
	/// The units of the Timestamps, when the track declared any. The cache keeps the track
	/// in its own units, so egress converts mutable ones to the units it declares.
	pub timescale: Option<Timescale>,
	/// The Timestamp the immutable block holds, if any, which keeps its ingress units.
	pub sealed_time: Option<u64>,
	/// Every property but the mutable Timestamp, if there are any.
	pub block: Option<Block>,
}

impl Properties {
	/// The object's time as moq-transport ingress takes it: the mutable Timestamp, or else
	/// the sealed one.
	pub fn time(&self) -> Option<u64> {
		self.timestamp.or(self.sealed_time)
	}
}

/// An object's properties other than its mutable Timestamp, as one encoded list.
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
	pub fn is_plain(&self) -> bool {
		self.block.is_none()
	}

	#[cfg(feature = "async")]
	pub fn is_empty(&self) -> bool {
		self.timestamp.is_none() && self.block.is_none()
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
