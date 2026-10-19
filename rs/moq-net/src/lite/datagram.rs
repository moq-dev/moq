//! Wire-level QUIC datagram body for moq-lite-05 (§6.4).
//!
//! One datagram carries a single-frame group routed over an existing subscription. The body is
//! `subscribe (i) | sequence (i) | [timestamp (i)] | [inflated size (i)] | payload (b)`, with the
//! timestamp present only when the subscription's track carries one (see
//! [`Version::wire_timescale`]) and the inflated size only when it is compressed (lite-07). The
//! payload runs to the datagram boundary, so unlike a [`super::Message`] there is no inner length
//! prefix. The model counterpart is [`crate::Datagram`].

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
	/// Absolute presentation timestamp in the track's wire timescale, or `None` on a track
	/// that carries no timestamps.
	pub timestamp: Option<u64>,
	/// On a compressed track, the payload's inflated size; the payload is then its own
	/// DEFLATE stream.
	pub inflated: Option<u64>,
	/// The frame payload, delimited by the datagram boundary.
	pub payload: Bytes,
}

/// What a datagram body carries beyond its payload, which the subscription's track decides
/// and the body itself does not say.
#[derive(Clone, Copy, Debug, Default)]
pub struct Format {
	/// The track carries a timestamp.
	pub timed: bool,
	/// The track is compressed, so the payload's inflated size precedes it.
	pub compressed: bool,
}

impl Encode<Version> for Datagram {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_datagrams() {
			return Err(EncodeError::Version);
		}

		// A version that always carries a timescale has no untimed body.
		if self.timestamp.is_none() && version.wire_timescale(None).is_some() {
			return Err(EncodeError::Version);
		}

		if self.inflated.is_some() && !version.has_compression() {
			return Err(EncodeError::Version);
		}

		w.varint(self.subscribe)?;
		w.varint(self.sequence)?;
		if let Some(timestamp) = self.timestamp {
			w.varint(timestamp)?;
		}
		if let Some(inflated) = self.inflated {
			w.varint(inflated)?;
		}

		// Payload runs to the datagram boundary: written raw, no length prefix.
		w.slice(&self.payload);
		Ok(())
	}
}

impl Datagram {
	/// Decode a whole datagram body. The payload shares `buf` rather than copying it.
	///
	/// `format` reports what the track of the given subscribe ID carries.
	pub fn decode(buf: Bytes, version: Version, format: impl FnOnce(u64) -> Format) -> Result<Self, DecodeError> {
		if !version.has_datagrams() {
			return Err(DecodeError::Version);
		}

		let mut r = Decoder::new(&buf, version.into());
		let subscribe = r.varint()?;
		let sequence = r.varint()?;
		let format = format(subscribe);
		let timestamp = if format.timed { Some(r.varint()?) } else { None };
		let inflated = match format.compressed && version.has_compression() {
			true => Some(r.varint()?),
			false => None,
		};

		// Everything remaining is the payload (the datagram boundary delimits it).
		let payload = buf.slice(buf.len() - r.remaining()..);

		Ok(Self {
			subscribe,
			sequence,
			timestamp,
			inflated,
			payload,
		})
	}
}

#[cfg(test)]
mod test {
	use super::*;

	const TIMED: Format = Format {
		timed: true,
		compressed: false,
	};

	/// A compressed track's inflated size sits between the timestamp and the payload.
	#[test]
	fn compressed_carries_the_inflated_size() {
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: Some(3),
			inflated: Some(40),
			payload: Bytes::from_static(b"slice"),
		};
		let buf = dg.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(&buf[..4], [1, 2, 3, 40]);
		let format = Format {
			timed: true,
			compressed: true,
		};
		assert_eq!(Datagram::decode(buf, Version::Lite07, |_| format).unwrap(), dg);

		for version in [Version::Lite05, Version::Lite06] {
			assert!(
				matches!(dg.encode_bytes(version), Err(EncodeError::Version)),
				"{version}"
			);
		}
	}

	#[test]
	fn roundtrip() {
		let original = Datagram {
			subscribe: 7,
			sequence: 42,
			timestamp: Some(1_000),
			inflated: None,
			payload: Bytes::from_static(b"hello"),
		};
		let buf = original.encode_bytes(Version::Lite05).unwrap();
		let decoded = Datagram::decode(buf, Version::Lite05, |_| TIMED).unwrap();
		assert_eq!(decoded, original, "payload has no trailing length prefix");
	}

	#[test]
	fn empty_payload() {
		let original = Datagram {
			subscribe: 0,
			sequence: 0,
			timestamp: Some(0),
			inflated: None,
			payload: Bytes::new(),
		};
		let buf = original.encode_bytes(Version::Lite05).unwrap();
		let decoded = Datagram::decode(buf, Version::Lite05, |_| TIMED).unwrap();
		assert_eq!(decoded, original);
	}

	#[test]
	fn no_inner_length_prefix() {
		// The payload is boundary-delimited, so the encoding is exactly the three
		// varints followed by the raw bytes (5 here) with nothing in between.
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: Some(3),
			inflated: None,
			payload: Bytes::from_static(b"world"),
		};
		let buf = dg.encode_bytes(Version::Lite05).unwrap();
		// 1 + 1 + 1 (single-byte varints) + 5 payload = 8 bytes, no length byte.
		assert_eq!(buf.len(), 8);
		assert_eq!(&buf[3..], b"world");
	}

	#[test]
	fn untimed_omits_the_timestamp() {
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: None,
			inflated: None,
			payload: Bytes::from_static(b"world"),
		};
		let buf = dg.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(buf.len(), 7);
		assert_eq!(&buf[2..], b"world");
		let decoded = Datagram::decode(buf, Version::Lite07, |id| {
			assert_eq!(id, 1);
			Format::default()
		})
		.unwrap();
		assert_eq!(decoded, dg);
	}

	#[test]
	fn untimed_needs_lite07() {
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: None,
			inflated: None,
			payload: Bytes::from_static(b"world"),
		};
		for version in [Version::Lite05, Version::Lite06] {
			assert!(
				matches!(dg.encode_bytes(version), Err(EncodeError::Version)),
				"{version}"
			);
		}
	}

	#[test]
	fn rejects_old_versions() {
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: Some(3),
			inflated: None,
			payload: Bytes::from_static(b"x"),
		};
		assert!(matches!(dg.encode_bytes(Version::Lite04), Err(EncodeError::Version)));

		assert!(matches!(
			Datagram::decode(Bytes::from_static(b"\x01\x02\x03x"), Version::Lite04, |_| TIMED),
			Err(DecodeError::Version)
		));
	}
}
