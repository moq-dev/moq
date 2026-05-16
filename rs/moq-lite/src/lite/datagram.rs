//! Wire-level datagram messages for moq-lite-04-datagrams+.
//!
//! Contains both the per-datagram QUIC datagram body codec ([Datagram]) and
//! the control-stream messages negotiated on the [`ControlType::Datagrams`]
//! bidirectional stream ([Datagrams], [DatagramsOk], [DatagramsUpdate]).
//!
//! [`ControlType::Datagrams`]: super::ControlType::Datagrams

use std::borrow::Cow;
use std::time::Duration;

use bytes::{Buf, BufMut, Bytes};

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

/// A single QUIC datagram body.
///
/// The encoding is `subscribe_id (i) | sequence (i) | payload (b)`. The QUIC
/// datagram boundary delimits the payload — there is no inner length prefix.
///
/// moq-lite-04-datagrams ignores the sequence number in delivery semantics; the field is
/// preserved on the wire so the same encoding can be reused by an
/// `moq-transport` adapter (deferred).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Datagram {
	pub subscribe: u64,
	pub sequence: u64,
	pub payload: Bytes,
}

impl Encode<Version> for Datagram {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(EncodeError::Version),
			_ => {}
		}

		self.subscribe.encode(w, version)?;
		self.sequence.encode(w, version)?;
		if w.remaining_mut() < self.payload.len() {
			return Err(EncodeError::Short);
		}
		w.put_slice(&self.payload);
		Ok(())
	}
}

impl Decode<Version> for Datagram {
	fn decode<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(DecodeError::Version),
			_ => {}
		}

		let subscribe = u64::decode(r, version)?;
		let sequence = u64::decode(r, version)?;
		let payload = r.copy_to_bytes(r.remaining());
		Ok(Self {
			subscribe,
			sequence,
			payload,
		})
	}
}

/// Sent by the subscriber to request datagram delivery for a track.
#[derive(Clone, Debug)]
pub struct Datagrams<'a> {
	pub id: u64,
	pub broadcast: Path<'a>,
	pub track: Cow<'a, str>,
	/// Maximum tolerated cache age in milliseconds. `Duration::ZERO` is strict:
	/// the publisher only forwards datagrams that the congestion controller
	/// can transmit immediately.
	pub max_latency: Duration,
}

impl Message for Datagrams<'_> {
	fn decode_msg<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(DecodeError::Version),
			_ => {}
		}

		Ok(Self {
			id: u64::decode(r, version)?,
			broadcast: Path::decode(r, version)?,
			track: Cow::<str>::decode(r, version)?,
			max_latency: Duration::decode(r, version)?,
		})
	}

	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(EncodeError::Version),
			_ => {}
		}

		self.id.encode(w, version)?;
		self.broadcast.encode(w, version)?;
		self.track.encode(w, version)?;
		self.max_latency.encode(w, version)?;
		Ok(())
	}
}

/// Publisher's acknowledgement of a [`Datagrams`] subscription.
#[derive(Clone, Debug)]
pub struct DatagramsOk {
	pub max_latency: Duration,
}

impl Message for DatagramsOk {
	fn decode_msg<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(DecodeError::Version),
			_ => {}
		}
		Ok(Self {
			max_latency: Duration::decode(r, version)?,
		})
	}

	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(EncodeError::Version),
			_ => {}
		}
		self.max_latency.encode(w, version)
	}
}

/// Subscriber updating an existing [`Datagrams`] subscription.
#[derive(Clone, Debug)]
pub struct DatagramsUpdate {
	pub max_latency: Duration,
}

impl Message for DatagramsUpdate {
	fn decode_msg<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(DecodeError::Version),
			_ => {}
		}
		Ok(Self {
			max_latency: Duration::decode(r, version)?,
		})
	}

	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => return Err(EncodeError::Version),
			_ => {}
		}
		self.max_latency.encode(w, version)
	}
}

#[cfg(test)]
mod test {
	use super::*;
	use bytes::BytesMut;

	#[test]
	fn datagram_roundtrip() {
		let original = Datagram {
			subscribe: 7,
			sequence: 42,
			payload: Bytes::from_static(b"hello"),
		};
		let mut buf = BytesMut::new();
		original.encode(&mut buf, Version::Lite04Datagrams).unwrap();
		let mut slice = &buf[..];
		let decoded = Datagram::decode(&mut slice, Version::Lite04Datagrams).unwrap();
		assert_eq!(decoded, original);
		assert!(!slice.has_remaining());
	}

	#[test]
	fn datagram_empty_payload() {
		let original = Datagram {
			subscribe: 0,
			sequence: 0,
			payload: Bytes::new(),
		};
		let mut buf = BytesMut::new();
		original.encode(&mut buf, Version::Lite04Datagrams).unwrap();
		let mut slice = &buf[..];
		let decoded = Datagram::decode(&mut slice, Version::Lite04Datagrams).unwrap();
		assert_eq!(decoded, original);
	}

	#[test]
	fn datagram_rejects_old_versions() {
		let original = Datagram {
			subscribe: 1,
			sequence: 2,
			payload: Bytes::from_static(b"x"),
		};
		let mut buf = BytesMut::new();
		assert!(matches!(
			original.encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn datagrams_message_roundtrip() {
		let original = Datagrams {
			id: 5,
			broadcast: Path::default(),
			track: Cow::Borrowed("video"),
			max_latency: Duration::from_millis(33),
		};
		let mut buf = BytesMut::new();
		original.encode(&mut buf, Version::Lite04Datagrams).unwrap();
		let decoded = Datagrams::decode(&mut buf, Version::Lite04Datagrams).unwrap();
		assert_eq!(decoded.id, original.id);
		assert_eq!(decoded.track, original.track);
		assert_eq!(decoded.max_latency, original.max_latency);
	}

	#[test]
	fn datagrams_ok_roundtrip() {
		let original = DatagramsOk {
			max_latency: Duration::from_millis(33),
		};
		let mut buf = BytesMut::new();
		original.encode(&mut buf, Version::Lite04Datagrams).unwrap();
		let decoded = DatagramsOk::decode(&mut buf, Version::Lite04Datagrams).unwrap();
		assert_eq!(decoded.max_latency, original.max_latency);
	}

	#[test]
	fn datagrams_update_roundtrip() {
		let original = DatagramsUpdate {
			max_latency: Duration::from_millis(0),
		};
		let mut buf = BytesMut::new();
		original.encode(&mut buf, Version::Lite04Datagrams).unwrap();
		let decoded = DatagramsUpdate::decode(&mut buf, Version::Lite04Datagrams).unwrap();
		assert_eq!(decoded.max_latency, original.max_latency);
	}

	#[test]
	fn datagrams_message_rejects_old_versions() {
		let original = Datagrams {
			id: 0,
			broadcast: Path::default(),
			track: Cow::Borrowed("x"),
			max_latency: Duration::ZERO,
		};
		let mut buf = BytesMut::new();
		assert!(matches!(
			original.encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}
}
