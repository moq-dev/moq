//! Wire-level QUIC datagram body for moq-lite-05 (§6.4).
//!
//! One datagram carries a single-frame group routed over an existing subscription. The body is
//! `subscribe (i) | sequence (i) | [timestamp (i)] | payload (b)`, with the timestamp present only
//! when the subscription's track carries one (see [`Version::wire_timescale`]). The payload runs to
//! the datagram boundary, so unlike a [`super::Message`] there is no inner length prefix. The model
//! counterpart is [`crate::Datagram`].

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
	/// The frame payload, delimited by the datagram boundary.
	pub payload: Bytes,
	/// With the `ietf` extension negotiated, the moq-transport state a relay preserves, or
	/// `Some(None)` for a plain datagram. `None` without the extension.
	pub ietf: Option<Option<crate::model::datagram::Wire>>,
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

		w.varint(self.subscribe)?;
		w.varint(self.sequence)?;
		if let Some(wire) = &self.ietf {
			if !version.has_ietf() {
				return Err(EncodeError::Version);
			}
			// A status carries no payload and no time.
			let status = wire
				.as_ref()
				.is_some_and(|wire| wire.status.is_some_and(|status| status != 0));
			if status && (self.timestamp.is_some() || !self.payload.is_empty()) {
				return Err(EncodeError::InvalidState);
			}
			super::ietf::encode_datagram(w, wire.as_ref(), version)?;
		}
		if let Some(timestamp) = self.timestamp {
			w.varint(timestamp)?;
		}

		// Payload runs to the datagram boundary: written raw, no length prefix.
		w.slice(&self.payload);
		Ok(())
	}
}

impl Datagram {
	/// Decode a whole datagram body. The payload shares `buf` rather than copying it.
	///
	/// `ietf` says whether the session negotiated the extension, and `timed` whether the
	/// track of the given subscribe ID carries a timestamp, which the body itself does not say.
	pub fn decode(
		buf: Bytes,
		version: Version,
		ietf: bool,
		timed: impl FnOnce(u64) -> bool,
	) -> Result<Self, DecodeError> {
		if !version.has_datagrams() || (ietf && !version.has_ietf()) {
			return Err(DecodeError::Version);
		}

		let mut r = Decoder::new(&buf, version.into());
		let subscribe = r.varint()?;
		let sequence = r.varint()?;
		let ietf = ietf
			.then(|| super::ietf::decode_datagram(&mut r, version))
			.transpose()?;
		// A status carries no payload and no time.
		let status = ietf
			.as_ref()
			.and_then(|wire| wire.as_ref())
			.is_some_and(|wire| wire.status.is_some_and(|status| status != 0));
		let timestamp = if timed(subscribe) && !status {
			Some(r.varint()?)
		} else {
			None
		};

		// Everything remaining is the payload (the datagram boundary delimits it).
		let payload = buf.slice(buf.len() - r.remaining()..);
		if status && !payload.is_empty() {
			return Err(DecodeError::InvalidValue);
		}

		Ok(Self {
			subscribe,
			sequence,
			timestamp,
			payload,
			ietf,
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
			timestamp: Some(1_000),
			payload: Bytes::from_static(b"hello"),
			ietf: None,
		};
		let buf = original.encode_bytes(Version::Lite05).unwrap();
		let decoded = Datagram::decode(buf, Version::Lite05, false, |_| true).unwrap();
		assert_eq!(decoded, original, "payload has no trailing length prefix");
	}

	#[test]
	fn empty_payload() {
		let original = Datagram {
			subscribe: 0,
			sequence: 0,
			timestamp: Some(0),
			payload: Bytes::new(),
			ietf: None,
		};
		let buf = original.encode_bytes(Version::Lite05).unwrap();
		let decoded = Datagram::decode(buf, Version::Lite05, false, |_| true).unwrap();
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
			payload: Bytes::from_static(b"world"),
			ietf: None,
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
			payload: Bytes::from_static(b"world"),
			ietf: None,
		};
		let buf = dg.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(buf.len(), 7);
		assert_eq!(&buf[2..], b"world");
		let decoded = Datagram::decode(buf, Version::Lite07, false, |id| {
			assert_eq!(id, 1);
			false
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
			payload: Bytes::from_static(b"world"),
			ietf: None,
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
			payload: Bytes::from_static(b"x"),
			ietf: None,
		};
		assert!(matches!(dg.encode_bytes(Version::Lite04), Err(EncodeError::Version)));

		assert!(matches!(
			Datagram::decode(Bytes::from_static(b"\x01\x02\x03x"), Version::Lite04, false, |_| true),
			Err(DecodeError::Version)
		));
	}
}
