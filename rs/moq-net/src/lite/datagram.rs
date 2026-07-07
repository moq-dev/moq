//! Wire-level QUIC datagram body for moq-lite-05 (§6.4).
//!
//! One datagram carries a single-frame group routed over an existing subscription. The body is
//! `subscribe (i) | sequence (i) | timestamp (i) | payload (b)`; the payload runs to the datagram
//! boundary, so unlike a [`super::Message`] there is no inner length prefix. The model counterpart
//! is [`crate::Datagram`].

use bytes::{Buf, BufMut, Bytes};

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

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
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_datagrams() {
			return Err(EncodeError::Version);
		}

		self.subscribe.encode(w, version)?;
		self.sequence.encode(w, version)?;
		self.timestamp.encode(w, version)?;

		// Payload runs to the datagram boundary: written raw, no length prefix.
		if w.remaining_mut() < self.payload.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(&self.payload);
		Ok(())
	}
}

impl Decode<Version> for Datagram {
	fn decode<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_datagrams() {
			return Err(DecodeError::Version);
		}

		let subscribe = u64::decode(r, version)?;
		let sequence = u64::decode(r, version)?;
		let timestamp = u64::decode(r, version)?;
		// Everything remaining is the payload (the datagram boundary delimits it).
		let payload = r.copy_to_bytes(r.remaining());

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
	use bytes::BytesMut;

	#[test]
	fn roundtrip() {
		let original = Datagram {
			subscribe: 7,
			sequence: 42,
			timestamp: 1_000,
			payload: Bytes::from_static(b"hello"),
		};
		let mut buf = BytesMut::new();
		original.encode(&mut buf, Version::Lite05).unwrap();
		let mut slice = &buf[..];
		let decoded = Datagram::decode(&mut slice, Version::Lite05).unwrap();
		assert_eq!(decoded, original);
		assert!(!slice.has_remaining(), "payload has no trailing length prefix");
	}

	#[test]
	fn empty_payload() {
		let original = Datagram {
			subscribe: 0,
			sequence: 0,
			timestamp: 0,
			payload: Bytes::new(),
		};
		let mut buf = BytesMut::new();
		original.encode(&mut buf, Version::Lite05).unwrap();
		let mut slice = &buf[..];
		let decoded = Datagram::decode(&mut slice, Version::Lite05).unwrap();
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
		let mut buf = BytesMut::new();
		assert!(matches!(
			dg.encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));

		let mut slice = &b"\x01\x02\x03x"[..];
		assert!(matches!(
			Datagram::decode(&mut slice, Version::Lite04),
			Err(DecodeError::Version)
		));
	}
}
