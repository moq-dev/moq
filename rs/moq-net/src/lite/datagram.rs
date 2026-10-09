//! Wire-level QUIC datagram body for moq-lite-05 (§6.4).
//!
//! One datagram carries a single-frame group routed over an existing subscription. The body is
//! `subscribe (i) | sequence (i) | timestamp (i) | payload (b)`; the payload runs to the datagram
//! boundary, so unlike a [`super::Message`] there is no inner length prefix. The model counterpart
//! is [`crate::Datagram`].

use bytes::Bytes;

use crate::coding::{DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::Version;

/// A decoded QUIC datagram body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// Subscribe ID this datagram is delivered on.
	pub subscribe: u64,
	/// Group sequence number (shared with the track's group namespace).
	pub sequence: u64,
	/// Absolute presentation timestamp, in the track's negotiated timescale.
	pub timestamp: u64,
	/// The frame payload, delimited by the datagram boundary.
	pub payload: Bytes,
}

impl Encode<Version> for Datagram {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_datagrams() {
			return Err(EncodeError::Version);
		}

		w.varint(self.subscribe)?;
		w.varint(self.sequence)?;
		w.varint(self.timestamp)?;

		// Payload runs to the datagram boundary: written raw, no length prefix.
		w.slice(&self.payload);
		Ok(())
	}
}

impl Datagram {
	/// Decode a whole datagram body. The payload shares `buf` rather than copying it.
	pub fn decode(buf: Bytes, version: Version) -> Result<Self, DecodeError> {
		if !version.has_datagrams() {
			return Err(DecodeError::Version);
		}

		let mut r = Decoder::new(&buf, version.into());
		let subscribe = r.varint()?;
		let sequence = r.varint()?;
		let timestamp = r.varint()?;

		// Everything remaining is the payload (the datagram boundary delimits it).
		let payload = buf.slice(buf.len() - r.remaining()..);

		Ok(Self {
			subscribe,
			sequence,
			timestamp,
			payload,
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn roundtrip() {
		let original = Datagram {
			subscribe: 7,
			sequence: 42,
			timestamp: 1_000,
			payload: Bytes::from_static(b"hello"),
		};
		let buf = original.encode_bytes(Version::Lite05).unwrap();
		let decoded = Datagram::decode(buf, Version::Lite05).unwrap();
		assert_eq!(decoded, original, "payload has no trailing length prefix");
	}

	#[test]
	fn empty_payload() {
		let original = Datagram {
			subscribe: 0,
			sequence: 0,
			timestamp: 0,
			payload: Bytes::new(),
		};
		let buf = original.encode_bytes(Version::Lite05).unwrap();
		let decoded = Datagram::decode(buf, Version::Lite05).unwrap();
		assert_eq!(decoded, original);
	}

	#[test]
	fn no_inner_length_prefix() {
		// The payload is boundary-delimited, so the encoding is exactly the three
		// varints followed by the raw bytes (5 here) with nothing in between.
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: 3,
			payload: Bytes::from_static(b"world"),
		};
		let buf = dg.encode_bytes(Version::Lite05).unwrap();
		// 1 + 1 + 1 (single-byte varints) + 5 payload = 8 bytes, no length byte.
		assert_eq!(buf.len(), 8);
		assert_eq!(&buf[3..], b"world");
	}

	#[test]
	fn rejects_old_versions() {
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: 3,
			payload: Bytes::from_static(b"x"),
		};
		assert!(matches!(dg.encode_bytes(Version::Lite04), Err(EncodeError::Version)));

		assert!(matches!(
			Datagram::decode(Bytes::from_static(b"\x01\x02\x03x"), Version::Lite04),
			Err(DecodeError::Version)
		));
	}
}
