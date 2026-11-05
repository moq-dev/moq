//! Stream identity, shared by both directions.

use std::fmt;

use crate::{Side, VarInt, coding};

/// Whether a stream communicates data in both directions or only from the initiator
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum Dir {
	/// Data flows in both directions
	Bi = 0,
	/// Data flows only from the stream's initiator
	Uni = 1,
}

impl Dir {
	pub(crate) fn iter() -> impl Iterator<Item = Self> {
		[Self::Bi, Self::Uni].iter().cloned()
	}
}

impl fmt::Display for Dir {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		use Dir::*;
		f.pad(match *self {
			Bi => "bidirectional",
			Uni => "unidirectional",
		})
	}
}

/// Identifier for a stream within a particular connection
#[derive(Debug, Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Id(pub(crate) u64);

impl fmt::Display for Id {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let initiator = match self.initiator() {
			Side::Client => "client",
			Side::Server => "server",
		};
		let dir = match self.dir() {
			Dir::Uni => "uni",
			Dir::Bi => "bi",
		};
		write!(f, "{} {}directional stream {}", initiator, dir, self.index())
	}
}

impl Id {
	/// Create a new stream ID
	pub fn new(initiator: Side, dir: Dir, index: u64) -> Self {
		Self((index << 2) | ((dir as u64) << 1) | initiator as u64)
	}
	/// Which side of a connection initiated the stream
	pub fn initiator(self) -> Side {
		if self.0 & 0x1 == 0 { Side::Client } else { Side::Server }
	}
	/// Which directions data flows in
	pub fn dir(self) -> Dir {
		if self.0 & 0x2 == 0 { Dir::Bi } else { Dir::Uni }
	}
	/// Distinguishes streams of the same initiator and directionality
	pub fn index(self) -> u64 {
		self.0 >> 2
	}
}

impl From<Id> for VarInt {
	fn from(x: Id) -> Self {
		unsafe { Self::from_u64_unchecked(x.0) }
	}
}

impl From<VarInt> for Id {
	fn from(v: VarInt) -> Self {
		Self(v.0)
	}
}

impl From<Id> for u64 {
	fn from(x: Id) -> Self {
		x.0
	}
}

impl coding::Codec for Id {
	fn decode<B: bytes::Buf>(buf: &mut B) -> coding::Result<Self> {
		VarInt::decode(buf).map(|x| Self(x.into_inner()))
	}
	fn encode<B: bytes::BufMut>(&self, buf: &mut B) {
		VarInt::from_u64(self.0).unwrap().encode(buf);
	}
}
