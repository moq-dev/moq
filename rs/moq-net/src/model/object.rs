//! Private relay metadata; public readers see only the frame or datagram payload.
use bytes::Bytes;

use crate::Timestamp;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Value {
	Int(u64),
	Bytes(Bytes),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Property {
	pub id: u64,
	pub value: Value,
}

/// An object's relay properties, held as one encoded [`Block`].
///
/// On a timed track, the mutable Timestamp is never held: it is the frame's, which egress
/// restates. On an untimed one it has no units, so it stays in the block like any other.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
pub(crate) struct Block {
	/// The properties in ascending ID order, serialized as the track's
	/// [`TrackProperties::version`] does. Never empty.
	pub bytes: Bytes,
	/// `bytes` holds Immutable Properties (0x0b).
	pub immutable: bool,
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

/// The moq-transport state of a track a moq-transport publisher published, which a relay
/// forwards unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrackProperties {
	/// The draft the publisher spoke, which serialized these and every object's [`Block`]
	/// and gives each property its meaning. A publisher on another draft is a new instance.
	pub version: crate::ietf::Version,
	/// The Track Properties moq-net does not interpret, in arrival order, which is
	/// ascending, with Immutable Properties (0x0b) as its serialized bytes.
	pub values: Vec<Property>,
}

/// Object numbering is separate from the public frame's ordinal in its group.
#[derive(Clone, Debug)]
pub(crate) struct Fields {
	pub id: u64,
	pub properties: Properties,
}

impl Fields {
	pub fn heap(&self) -> u64 {
		(size_of::<Self>() + 2 * size_of::<usize>()) as u64 + self.properties.heap()
	}
}
