//! Decrypted datagrams and the events a protected datagram read yields.

use bytes::Bytes;

/// A decrypted datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// Per-track sequence, shared with the group namespace.
	pub sequence: u64,
	/// Presentation timestamp, or `None` when untimed.
	pub timestamp: Option<moq_net::Timestamp>,
	/// Decrypted application bytes.
	pub plaintext: Bytes,
}

/// Outcome of reading one protected datagram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
	/// Authenticated plaintext.
	Datagram(Datagram),
	/// The datagram failed to authenticate; dropped, track continues.
	Authentication {
		/// Sequence of the rejected datagram.
		sequence: u64,
	},
	/// Sequence already opened inside the retained window; dropped, track continues.
	Duplicate {
		/// Sequence of the duplicate datagram.
		sequence: u64,
	},
	/// The sequence is above `2^53-1`; dropped, track continues.
	Identity {
		/// Sequence of the rejected datagram.
		sequence: u64,
	},
	/// The payload is shorter than the tag or longer than the datagram cap; dropped, track continues.
	Oversize {
		/// Sequence of the rejected datagram.
		sequence: u64,
	},
}
