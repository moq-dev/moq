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

/// The common timestamp needs no vector allocation; all other properties stay opaque.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
// private: passthrough
pub(crate) struct Properties {
	pub timestamp: Option<u64>,
	pub values: Vec<Property>,
	pub immutable: Option<Encoding>,
	/// The units of the Timestamps above, when the track declared any. The cache keeps the
	/// track in its own units, so egress converts mutable ones to the units it declares.
	pub timescale: Option<Timescale>,
	/// The immutable block holds a Timestamp, which keeps its ingress units.
	pub sealed_time: bool,
	/// Property 0x06 means SUBGROUP_DELIVERY_TIMEOUT on the ingress draft.
	pub delivery_timeout: bool,
}

impl Properties {
	#[cfg(feature = "async")]
	pub fn is_plain(&self) -> bool {
		self.values.is_empty()
	}

	#[cfg(feature = "async")]
	pub fn is_empty(&self) -> bool {
		self.timestamp.is_none() && self.values.is_empty()
	}

	pub fn heap(&self) -> u64 {
		(self.values.capacity() * size_of::<Property>()) as u64
			+ self
				.values
				.iter()
				.map(|property| match &property.value {
					Value::Bytes(bytes) => bytes.len() as u64,
					Value::Int(_) => 0,
				})
				.sum::<u64>()
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
	/// The keys inside Immutable Properties, which egress must not restate as mutable.
	pub sealed: Vec<u64>,
	/// Property 0x06 means SUBGROUP_DELIVERY_TIMEOUT on the ingress draft.
	pub delivery_timeout: bool,
}

impl TrackProperties {
	/// Whether one is a Mandatory Track Property (0x4000-0x7FFF), which by definition is
	/// one moq-net does not understand: the track must be neither processed nor forwarded.
	#[cfg_attr(not(feature = "async"), allow(dead_code))]
	pub fn mandatory(&self) -> bool {
		const MANDATORY: std::ops::RangeInclusive<u64> = 0x4000..=0x7fff;
		self.values.iter().any(|property| MANDATORY.contains(&property.id))
			|| self.sealed.iter().any(|id| MANDATORY.contains(id))
	}
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
