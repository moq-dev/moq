//! Decrypted datagrams and the events a protected datagram read yields.

use bytes::Bytes;

/// A decrypted datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// Per-track sequence, shared with the group namespace.
	pub sequence: u64,
	/// Presentation timestamp.
	pub timestamp: moq_net::Timestamp,
	/// Decrypted application bytes.
	pub plaintext: Bytes,
}

/// Outcome of reading one protected datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	/// Authenticated plaintext.
	Datagram(Datagram),
	/// AEAD open failed; the datagram was dropped and the track continues.
	Authentication {
		/// Sequence of the rejected datagram.
		sequence: u64,
	},
	/// Sequence already opened inside the retained window; dropped, track continues.
	Duplicate {
		/// Sequence of the duplicate datagram.
		sequence: u64,
	},
}
