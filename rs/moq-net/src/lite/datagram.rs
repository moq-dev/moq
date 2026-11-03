//! Wire-level QUIC datagram bodies for moq-lite.
//!
//! One datagram carries a single-frame group routed over an existing subscription. The body is
//! `subscribe (i) | sequence (i) | [timestamp (i)] | [inflated size (i)] | payload (b)`, with the
//! timestamp present only when the subscription's track carries one (see
//! [`Version::wire_timescale`]) and the inflated size only when it is compressed (lite-07).
//! Lite-07 prefixes group datagrams with type 0x0; type 0x1 is discarded padding. The
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

		if self.inflated.is_some() && !version.has_compression() {
			return Err(EncodeError::Version);
		}

		if version.has_padding() {
			w.varint(0x0)?;
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
			if status.is_some() && (!self.payload.is_empty() || self.inflated.is_some())
				|| status.is_some_and(|status| status != 0) && self.timestamp.is_some()
			{
				return Err(EncodeError::InvalidState);
			}
			super::ietf::encode_datagram(w, wire.as_ref())?;
		}
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
	/// The largest encoded datagram body a lite hop carries.
	pub(super) const MAX_SIZE: usize = 1200;

	/// Decode a group datagram, or discard padding with `None`, without copying its payload.
	///
	/// `ietf` says whether the session negotiated the extension, and `framing` describes
	/// the track of the given subscribe ID, which the body itself does not.
	pub(crate) fn decode(
		buf: Bytes,
		version: Version,
		ietf: bool,
		framing: impl FnOnce(u64) -> super::Framing,
	) -> Result<Option<Self>, DecodeError> {
		if !version.has_datagrams() || (ietf && !version.has_ietf()) {
			return Err(DecodeError::Version);
		}

		let mut r = Decoder::new(&buf, version.into());
		if version.has_padding() {
			match r.varint()? {
				0x0 => (),
				0x1 => return Ok(None),
				_ => return Err(DecodeError::InvalidValue),
			}
		}
		let subscribe = r.varint()?;
		let sequence = r.varint()?;
		let framing = framing(subscribe);
		let timescale = framing.timescale;
		let ietf = ietf
			.then(|| super::ietf::decode_datagram(&mut r, framing))
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
		// A status has no payload to inflate.
		let inflated = match framing.compressed && version.has_compression() && status.is_none() {
			true => Some(r.varint()?),
			false => None,
		};

		// Everything remaining is the payload (the datagram boundary delimits it).
		let payload = buf.slice(buf.len() - r.remaining()..);
		if status.is_some() && !payload.is_empty() {
			return Err(DecodeError::InvalidValue);
		}

		Ok(Some(Self {
			subscribe,
			sequence,
			timestamp,
			inflated,
			payload,
			ietf,
		}))
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use crate::Timescale;

	#[test]
	fn padding_needs_no_subscription_framing() {
		for mut body in [vec![1], vec![1; Datagram::MAX_SIZE], vec![0x80, 1, 0xff]] {
			// Padding bytes need not be valid varints or any other structured encoding.
			if body.len() == Datagram::MAX_SIZE {
				body[1..].fill(0xff);
			}
			for ietf in [false, true] {
				assert!(
					Datagram::decode(body.clone().into(), Version::Lite07, ietf, |_| {
						panic!("padding must not look up a subscription")
					})
					.unwrap()
					.is_none()
				);
			}
		}
	}

	#[test]
	fn invalid_types_never_reach_a_subscription() {
		for body in [&[][..], &[0x80][..], &[2, 1, 2, 3][..], &[0xff; 9][..]] {
			assert!(
				Datagram::decode(Bytes::copy_from_slice(body), Version::Lite07, false, |_| {
					panic!("invalid type must not look up a subscription")
				})
				.is_err()
			);
		}
	}

	/// A millisecond track a moq-lite publisher published.
	fn timed() -> super::super::Framing {
		super::super::Framing {
			timescale: Some(Timescale::MILLI),
			ietf: None,
			compressed: false,
		}
	}

	/// A millisecond track a draft-22 moq-transport publisher published.
	fn relayed() -> super::super::Framing {
		super::super::Framing {
			ietf: Some(crate::ietf::Version::Draft22),
			..timed()
		}
	}

	/// A millisecond compressed track a moq-lite publisher published.
	fn compressed() -> super::super::Framing {
		super::super::Framing {
			compressed: true,
			..timed()
		}
	}

	/// A compressed track's inflated size sits between the timestamp and the payload.
	#[test]
	fn compressed_carries_the_inflated_size() {
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: Some(3),
			inflated: Some(40),
			payload: Bytes::from_static(b"slice"),
			ietf: None,
		};
		let buf = dg.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(&buf[..5], [0, 1, 2, 3, 40]);
		assert_eq!(
			Datagram::decode(buf, Version::Lite07, false, |_| compressed())
				.unwrap()
				.unwrap(),
			dg
		);

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
			subscribe: 1,
			sequence: 42,
			timestamp: Some(1_000),
			inflated: None,
			payload: Bytes::from_static(b"hello"),
			ietf: None,
		};
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let buf = original.encode_bytes(version).unwrap();
			let decoded = Datagram::decode(buf, version, false, |_| timed()).unwrap().unwrap();
			assert_eq!(decoded, original, "payload has no trailing length prefix");
		}
	}

	#[test]
	fn empty_payload() {
		let original = Datagram {
			subscribe: 0,
			sequence: 0,
			timestamp: Some(0),
			inflated: None,
			payload: Bytes::new(),
			ietf: None,
		};
		let buf = original.encode_bytes(Version::Lite05).unwrap();
		let decoded = Datagram::decode(buf, Version::Lite05, false, |_| timed())
			.unwrap()
			.unwrap();
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
			inflated: None,
			payload: Bytes::from_static(b"world"),
			ietf: None,
		};
		let buf = dg.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(buf.len(), 8);
		assert_eq!(&buf[3..], b"world");
		let decoded = Datagram::decode(buf, Version::Lite07, false, |id| {
			assert_eq!(id, 1);
			Default::default()
		})
		.unwrap()
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
			inflated: None,
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
						crate::ietf::Version::Draft22,
					)
					.unwrap(),
					..Default::default()
				},
			})),
		};
		let decode = |dg: Datagram| {
			let buf = dg.encode_bytes(Version::Lite07).unwrap();
			Datagram::decode(buf, Version::Lite07, true, |_| relayed())
		};
		assert!(decode(dg(0x20)).is_ok());
		assert!(matches!(decode(dg(0x10)), Err(DecodeError::InvalidValue)));
	}

	/// A Normal status (0) is an empty object, which moq-transport sends as a status with
	/// no payload, so a STATUS field beside a payload, or its inflated size, is refused
	/// whatever its value.
	#[test]
	fn a_status_carries_no_payload() {
		for status in [0, 3] {
			let dg = |payload: &'static [u8]| Datagram {
				subscribe: 1,
				sequence: 2,
				timestamp: (status == 0).then_some(500),
				inflated: None,
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
			let sized = Datagram {
				inflated: Some(0),
				..empty.clone()
			};
			assert!(
				matches!(sized.encode_bytes(Version::Lite07), Err(EncodeError::InvalidState)),
				"{status}"
			);
			let mut buf = empty.encode_bytes(Version::Lite07).unwrap().to_vec();
			for framing in [
				relayed(),
				super::super::Framing {
					compressed: true,
					..relayed()
				},
			] {
				let decoded = Datagram::decode(Bytes::from(buf.clone()), Version::Lite07, true, |_| framing)
					.unwrap()
					.unwrap();
				assert_eq!(decoded, empty, "{status}");
			}
			buf.push(b'x');
			assert!(
				matches!(
					Datagram::decode(Bytes::from(buf), Version::Lite07, true, |_| relayed()),
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
			inflated: None,
			payload: Bytes::from_static(b"x"),
			ietf: None,
		};
		assert!(matches!(dg.encode_bytes(Version::Lite04), Err(EncodeError::Version)));

		assert!(matches!(
			Datagram::decode(Bytes::from_static(b"\x01\x02\x03x"), Version::Lite04, false, |_| timed(
			)),
			Err(DecodeError::Version)
		));
	}
}
