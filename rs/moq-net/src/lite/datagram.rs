//! Wire-level QUIC datagram body for moq-lite-05 (§6.4).
//!
//! One datagram carries a single-frame group routed over an existing subscription. The body is
//! `subscribe (i) | sequence (i) | [timestamp (i)] | payload (b)`, with the timestamp present only
//! when the subscription's track carries one (see [`Version::wire_timescale`]). The payload runs to
//! the datagram boundary, so unlike a [`super::Message`] there is no inner length prefix. The model
//! counterpart is [`crate::Datagram`].

use crate::Timescale;
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
			// Any status object carries no payload, which moq-transport could not send beside
			// it, and a non-zero one carries no object and so no time either.
			let status = wire.as_ref().and_then(|wire| wire.status);
			if status.is_some() && !self.payload.is_empty()
				|| status.is_some_and(|status| status != 0) && self.timestamp.is_some()
			{
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
	/// `ietf` says whether the session negotiated the extension, and `timescale` gives the
	/// units of the track of the given subscribe ID, if it is timed, which the body itself
	/// does not say.
	pub fn decode(
		buf: Bytes,
		version: Version,
		ietf: bool,
		timescale: impl FnOnce(u64) -> Option<Timescale>,
	) -> Result<Self, DecodeError> {
		if !version.has_datagrams() || (ietf && !version.has_ietf()) {
			return Err(DecodeError::Version);
		}

		let mut r = Decoder::new(&buf, version.into());
		let subscribe = r.varint()?;
		let sequence = r.varint()?;
		let timescale = timescale(subscribe);
		let ietf = ietf
			.then(|| super::ietf::decode_datagram(&mut r, timescale, version))
			.transpose()?;
		// Any status object carries no payload, which moq-transport could not send beside
		// it, and a non-zero one carries no object and so no time either.
		let status = ietf
			.as_ref()
			.and_then(|wire| wire.as_ref())
			.and_then(|wire| wire.status);
		let timestamp = if timescale.is_some() && status.is_none_or(|status| status == 0) {
			Some(r.varint()?)
		} else {
			None
		};

		// Everything remaining is the payload (the datagram boundary delimits it).
		let payload = buf.slice(buf.len() - r.remaining()..);
		if status.is_some() && !payload.is_empty() {
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
		let decoded = Datagram::decode(buf, Version::Lite05, false, |_| Some(Timescale::MILLI)).unwrap();
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
		let decoded = Datagram::decode(buf, Version::Lite05, false, |_| Some(Timescale::MILLI)).unwrap();
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
			None
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

	/// A timed datagram's Timestamp is its only time: its relayed properties cannot hold a
	/// second, mutable one that could disagree.
	#[test]
	fn relayed_properties_hold_no_timestamp_on_a_timed_track() {
		let dg = |id: u64| Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: Some(500),
			payload: Bytes::from_static(b"x"),
			ietf: Some(Some(crate::model::datagram::Wire {
				object_id: None,
				priority: None,
				end_of_group: false,
				status: None,
				properties: crate::model::object::Properties {
					block: crate::ietf::object::block(
						&[crate::model::object::Property {
							id,
							value: crate::model::object::Value::Int(7),
						}],
						crate::model::object::Encoding::Draft19,
					)
					.unwrap(),
					..Default::default()
				},
			})),
		};
		let decode = |dg: Datagram| {
			let buf = dg.encode_bytes(Version::Lite07).unwrap();
			Datagram::decode(buf, Version::Lite07, true, |_| Some(Timescale::MILLI))
		};
		assert!(decode(dg(0x20)).is_ok());
		assert!(matches!(decode(dg(0x10)), Err(DecodeError::InvalidValue)));
	}

	/// A Normal status (0) is an empty object, which moq-transport sends as a status with
	/// no payload, so a STATUS field beside a payload is refused whatever its value.
	#[test]
	fn a_status_carries_no_payload() {
		for status in [0, 3] {
			let dg = |payload: &'static [u8]| Datagram {
				subscribe: 1,
				sequence: 2,
				timestamp: (status == 0).then_some(500),
				payload: Bytes::from_static(payload),
				ietf: Some(Some(crate::model::datagram::Wire {
					object_id: None,
					priority: None,
					end_of_group: false,
					status: Some(status),
					properties: Default::default(),
				})),
			};
			assert!(
				matches!(dg(b"x").encode_bytes(Version::Lite07), Err(EncodeError::InvalidState)),
				"{status}"
			);
			let empty = dg(b"");
			let mut buf = empty.encode_bytes(Version::Lite07).unwrap().to_vec();
			let decoded = Datagram::decode(Bytes::from(buf.clone()), Version::Lite07, true, |_| {
				Some(Timescale::MILLI)
			})
			.unwrap();
			assert_eq!(decoded, empty, "{status}");
			buf.push(b'x');
			assert!(
				matches!(
					Datagram::decode(Bytes::from(buf), Version::Lite07, true, |_| Some(Timescale::MILLI)),
					Err(DecodeError::InvalidValue)
				),
				"{status}"
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
			Datagram::decode(Bytes::from_static(b"\x01\x02\x03x"), Version::Lite04, false, |_| Some(
				Timescale::MILLI
			)),
			Err(DecodeError::Version)
		));
	}
}
