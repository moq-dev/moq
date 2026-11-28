//! moq-lite datagrams: QUIC datagram bodies, and lite-07's request for a datagram track.
//!
//! A body is `subscribe (i) | sequence (i) | timestamp (i) | payload (b)`. Lite-07 prefixes it
//! with type 0x0, its only datagram type.
//! The payload runs to the datagram boundary, so unlike a [`super::Message`] there is no
//! inner length prefix.
//!
//! On lite-05/06 a body is a single-frame group on a SUBSCRIBE subscription, its sequence
//! the group's. On lite-07 it is a datagram of a datagram track, subscribed through a
//! Datagrams Stream ([`Datagrams`]), its sequence the datagram track's own. The model
//! counterpart is [`crate::datagram::Datagram`].

use std::borrow::Cow;

use bytes::Bytes;

use crate::{
	Path, Timescale,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
};

use super::{Message, Version};

/// A decoded QUIC datagram body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	/// Subscribe ID this datagram is delivered on.
	pub subscribe: u64,
	/// The datagram's sequence: its group's on lite-05/06, the datagram track's on lite-07.
	pub sequence: u64,
	/// Absolute presentation timestamp in the track's wire timescale. A scale-less track's
	/// is the bare value its producer gave, which is the datagram's sequence (see
	/// [`Version::wire_timescale`]).
	pub timestamp: u64,
	/// The payload, delimited by the datagram boundary.
	pub payload: Bytes,
}

impl Encode<Version> for Datagram {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_datagrams() {
			return Err(EncodeError::Version);
		}

		if version.has_datagram_tracks() {
			w.varint(0x0)?;
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
	/// The largest encoded datagram body a lite hop carries.
	pub(super) const MAX_SIZE: usize = 1200;

	/// Decode a datagram without copying its payload.
	///
	/// An unknown lite-07 type is an error, so the caller discards it.
	pub(crate) fn decode(buf: Bytes, version: Version) -> Result<Self, DecodeError> {
		if !version.has_datagrams() {
			return Err(DecodeError::Version);
		}

		let mut r = Decoder::new(&buf, version.into());
		if version.has_datagram_tracks() && r.varint()? != 0x0 {
			return Err(DecodeError::InvalidValue);
		}
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

/// Sent by the subscriber on a Datagrams Stream (0x9) to subscribe to a datagram track,
/// live from now.
///
/// Lite07+ only.
#[derive(Clone, Debug)]
pub struct Datagrams<'a> {
	/// The Subscribe ID its datagrams carry, from the same space as SUBSCRIBE's.
	pub id: u64,
	pub broadcast: Path,
	/// The identity the subscriber expects; see [`crate::origin::Route::identity`].
	pub identity: Option<crate::Identity>,
	/// The forwarding node whose claim the request follows, or `None` for whichever the
	/// publisher selects. Lite07+ only.
	pub node: Option<crate::route::NodeId>,
	pub track: Cow<'a, str>,
	/// The subscriber's priority.
	pub priority: i8,
}

impl Datagrams<'_> {
	/// Copy the borrowed track name, so the request outlives the bytes it came from.
	pub fn into_owned(self) -> Datagrams<'static> {
		Datagrams {
			id: self.id,
			broadcast: self.broadcast,
			identity: self.identity,
			node: self.node,
			track: Cow::Owned(self.track.into_owned()),
			priority: self.priority,
		}
	}
}

impl<'a> Message<'a> for Datagrams<'a> {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_datagram_tracks() {
			return Err(DecodeError::Version);
		}
		let id = r.varint()?;
		let broadcast = Path::decode(r, version)?;
		let identity = super::identity::decode_pin(r, version)?;
		let node = super::identity::decode_node(r, version)?;
		let track = Cow::Borrowed(r.str()?);
		let priority = super::priority::from_wire(r.u8()?);
		Ok(Self {
			id,
			broadcast,
			identity,
			node,
			track,
			priority,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_datagram_tracks() {
			return Err(EncodeError::Version);
		}
		w.varint(self.id)?;
		self.broadcast.encode(w, version)?;
		super::identity::encode_pin(w, version, self.identity.as_ref())?;
		super::identity::encode_node(w, version, self.node)?;
		w.string(&self.track)?;
		w.u8(super::priority::to_wire(self.priority));
		Ok(())
	}
}

/// Sent by the subscriber on a Datagrams Stream to change its priority.
///
/// Lite07+ only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatagramsUpdate {
	pub priority: i8,
}

impl<'a> Message<'a> for DatagramsUpdate {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Subscribe);

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_datagram_tracks() {
			return Err(DecodeError::Version);
		}
		let priority = super::priority::from_wire(r.u8()?);
		Ok(Self { priority })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_datagram_tracks() {
			return Err(EncodeError::Version);
		}
		w.u8(super::priority::to_wire(self.priority));
		Ok(())
	}
}

/// The publisher's answer on a Datagrams Stream: the datagram track's properties, which a
/// subscriber needs to read its datagrams.
///
/// Lite07+ only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatagramsInfo {
	/// The publisher's tie-break priority for this track.
	pub priority: i8,
	/// Units per second of each datagram's timestamp, or `None` when datagrams carry none.
	pub timescale: Option<Timescale>,
}

impl From<crate::datagram::Info> for DatagramsInfo {
	fn from(info: crate::datagram::Info) -> Self {
		Self {
			priority: info.priority,
			timescale: info.timescale,
		}
	}
}

impl From<DatagramsInfo> for crate::datagram::Info {
	fn from(info: DatagramsInfo) -> Self {
		Self::default()
			.with_priority(info.priority)
			.with_timescale(info.timescale)
	}
}

impl<'a> Message<'a> for DatagramsInfo {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::TrackInfo);

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_datagram_tracks() {
			return Err(DecodeError::Version);
		}
		let priority = super::priority::from_wire(r.u8()?);
		let timescale = match r.varint()? {
			0 => None,
			scale => Some(Timescale::new(scale).map_err(|_| DecodeError::InvalidValue)?),
		};
		Ok(Self { priority, timescale })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_datagram_tracks() {
			return Err(EncodeError::Version);
		}
		w.u8(super::priority::to_wire(self.priority));
		w.varint(self.timescale.map_or(0, u64::from))?;
		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn invalid_types_never_reach_a_subscription() {
		// 0x0 is the only type.
		for body in [
			&[][..],
			&[0x80][..],
			&[1, 1, 2, 3][..],
			&[2, 1, 2, 3][..],
			&[0xff; 9][..],
		] {
			assert!(Datagram::decode(Bytes::copy_from_slice(body), Version::Lite07).is_err());
		}
	}

	#[test]
	fn roundtrip() {
		let original = Datagram {
			subscribe: 1,
			sequence: 42,
			timestamp: 1_000,
			payload: Bytes::from_static(b"hello"),
		};
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			let buf = original.encode_bytes(version).unwrap();
			let decoded = Datagram::decode(buf, version).unwrap();
			assert_eq!(decoded, original);
		}
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
		assert_eq!(buf.len(), 8);
		assert_eq!(&buf[3..], b"world");
	}

	#[test]
	fn a_scale_less_lite07_datagram_carries_its_bare_timestamp() {
		// A scale-less track's timestamp is the producer's bare value, still on the wire.
		let dg = Datagram {
			subscribe: 1,
			sequence: 2,
			timestamp: 2,
			payload: Bytes::from_static(b"world"),
		};
		let buf = dg.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(&buf[..], b"\x00\x01\x02\x02world");
		assert_eq!(Datagram::decode(buf, Version::Lite07).unwrap(), dg);
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

	#[test]
	fn the_request_roundtrips_on_lite07_only() {
		let request = Datagrams {
			id: 7,
			broadcast: Path::from_static("room/alice"),
			identity: None,
			node: Some(crate::route::NodeId {
				relay: 0x7a3f,
				marked: false,
			}),
			track: "audio".into(),
			priority: -3,
		};
		let buf = request.encode_bytes(Version::Lite07).unwrap();
		let (decoded, _) = Datagrams::decode_slice(&buf, Version::Lite07).unwrap();
		assert_eq!(decoded.id, 7);
		assert_eq!(decoded.broadcast, request.broadcast);
		assert_eq!(
			decoded.node,
			Some(crate::route::NodeId {
				relay: 0x7a3f,
				marked: false,
			})
		);
		assert_eq!(decoded.track, "audio");
		assert_eq!(decoded.priority, -3);
		assert!(matches!(
			request.encode_bytes(Version::Lite06),
			Err(EncodeError::Version)
		));

		let update = DatagramsUpdate { priority: 9 };
		let buf = update.encode_bytes(Version::Lite07).unwrap();
		assert_eq!(DatagramsUpdate::decode_slice(&buf, Version::Lite07).unwrap().0, update);

		for timescale in [None, Some(Timescale::MILLI)] {
			let info = DatagramsInfo { priority: 2, timescale };
			let buf = info.encode_bytes(Version::Lite07).unwrap();
			assert_eq!(DatagramsInfo::decode_slice(&buf, Version::Lite07).unwrap().0, info);
		}
	}
}
