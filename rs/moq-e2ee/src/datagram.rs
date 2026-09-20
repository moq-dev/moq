//! Datagram plaintext, events, and payload-limit helper.

use bytes::Bytes;

use crate::error::Result;
use crate::limits::datagram_payload_limit;

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
	/// Identity already opened inside the retained window; dropped, track continues.
	Duplicate {
		/// Sequence of the duplicate datagram.
		sequence: u64,
	},
}

/// Insert ciphertext at an explicit sequence on the net track.
pub(crate) fn insert_ciphertext(
	track: &mut moq_net::track::Producer,
	sequence: u64,
	timestamp: moq_net::Timestamp,
	payload: Bytes,
) -> moq_net::Result<()> {
	track.insert_datagram(sequence, timestamp, payload)
}

/// Ciphertext budget for a datagram that will encode these fields.
///
/// # Errors
///
/// [`Error::Identity`](crate::Error::Identity) if a field cannot be a QUIC varint.
pub fn payload_limit(subscribe: u64, sequence: u64, timestamp: u64) -> Result<usize> {
	datagram_payload_limit(subscribe, sequence, timestamp)
}
