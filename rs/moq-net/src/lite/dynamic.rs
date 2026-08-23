use bytes::{Buf, BufMut};

use crate::{Origin, OriginList, Path, Pattern, coding::*};

use super::{Message, Version};

// Dynamic Stream message types: an outer discriminator carried before the length
// prefix, so each advertisement is an independently-typed, length-delimited
// message (mirroring the Announce Stream's lite-06 framing).
const DYNAMIC_START: u64 = 0;
const DYNAMIC_END: u64 = 1;
const DYNAMIC_UPDATE: u64 = 2;

/// DYNAMIC_REQUEST: sent by the subscriber as the first message on a Dynamic
/// Stream, requesting every dynamic advertisement whose pattern can match a
/// path starting with the requested prefix.
///
/// Encoded exactly as ANNOUNCE_REQUEST's lite-06 form; the stream only exists
/// on versions where the request carries no exclude_hop.
#[derive(Clone, Debug)]
pub struct DynamicRequest<'a> {
	pub prefix: Path<'a>,
}

impl Message for DynamicRequest<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let prefix = Path::decode(r, version)?;
		Ok(Self { prefix })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.prefix.encode(w, version)
	}
}

/// DYNAMIC_OK: sent by the publisher exactly once, as the first message on the
/// response side of a Dynamic Stream. Fields mirror ANNOUNCE_OK: the
/// publisher's own hop id (the implicit trailing entry of every
/// advertisement's hop list) and the size of the initial burst.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DynamicOk {
	pub origin: Origin,
	pub active: u64,
}

impl Message for DynamicOk {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Ok(Self {
			origin: Origin::decode(r, version)?,
			active: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.origin.encode(w, version)?;
		self.active.encode(w, version)
	}
}

/// An advertisement on the Dynamic Stream: a pattern of paths the publisher
/// could serve on demand, retracted or replaced by its implicit Dynamic ID.
///
/// The pattern's prefix is carried whole (relative to the session's namespace,
/// like every path); the stream's requested prefix decides WHICH patterns the
/// stream carries, never how one is encoded. The single Route Cost stands in
/// for both the Warm and Cold halves: a pattern names no carried content, so
/// it is never warm and the two would be provably equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DynamicAdvert<'a> {
	/// DYNAMIC_START: a pattern the publisher could serve. Assigns the next
	/// Dynamic ID (a per-stream ordinal starting at 0).
	Start {
		pattern: Pattern<'a>,
		hops: OriginList,
		cost: u64,
	},
	/// DYNAMIC_END: the advertisement with this id is retracted; the id is
	/// retired.
	EndId { id: u64 },
	/// DYNAMIC_UPDATE: atomically replace the advertisement with this id
	/// (e.g. a new hop chain, or a cost that moved). The id stays live.
	Update { id: u64, hops: OriginList, cost: u64 },
}

impl Encode<Version> for DynamicAdvert<'_> {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_dynamic() {
			return Err(EncodeError::Version);
		}

		// Outer type discriminator, then a size-prefixed body, like the announce
		// stream. Advertisements are small and infrequent, so the scratch buffer
		// is cheap.
		let mut body = Vec::new();
		let typ = match self {
			Self::Start { pattern, hops, cost } => {
				pattern.prefix.encode(&mut body, version)?;
				pattern.suffix.encode(&mut body, version)?;
				hops.encode(&mut body, version)?;
				cost.encode(&mut body, version)?;
				DYNAMIC_START
			}
			Self::EndId { id } => {
				id.encode(&mut body, version)?;
				DYNAMIC_END
			}
			Self::Update { id, hops, cost } => {
				id.encode(&mut body, version)?;
				hops.encode(&mut body, version)?;
				cost.encode(&mut body, version)?;
				DYNAMIC_UPDATE
			}
		};
		typ.encode(w, version)?;
		(body.len() as u64).encode(w, version)?;
		w.put_slice(&body);
		Ok(())
	}
}

impl Decode<Version> for DynamicAdvert<'_> {
	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		if !version.has_dynamic() {
			return Err(DecodeError::InvalidValue);
		}

		let typ = u64::decode(buf, version)?;
		let size = usize::decode(buf, version)?;
		if buf.remaining() < size {
			return Err(DecodeError::Short);
		}
		let mut body = buf.take(size);
		let msg = match typ {
			DYNAMIC_START => Self::Start {
				pattern: Pattern {
					prefix: Path::decode(&mut body, version)?,
					suffix: Path::decode(&mut body, version)?,
				},
				hops: OriginList::decode(&mut body, version)?,
				cost: u64::decode(&mut body, version)?,
			},
			DYNAMIC_END => Self::EndId {
				id: u64::decode(&mut body, version)?,
			},
			DYNAMIC_UPDATE => Self::Update {
				id: u64::decode(&mut body, version)?,
				hops: OriginList::decode(&mut body, version)?,
				cost: u64::decode(&mut body, version)?,
			},
			_ => return Err(DecodeError::InvalidMessage(typ)),
		};
		if body.remaining() > 0 {
			return Err(DecodeError::Long);
		}
		Ok(msg)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn advert_round_trip(msg: &DynamicAdvert, version: Version) -> DynamicAdvert<'static> {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, version).unwrap();
		let mut slice = &buf[..];
		let got = DynamicAdvert::decode(&mut slice, version).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		// Decode borrows from `buf`; re-own so the value can outlive this frame.
		match got {
			DynamicAdvert::Start { pattern, hops, cost } => DynamicAdvert::Start {
				pattern: pattern.to_owned(),
				hops,
				cost,
			},
			DynamicAdvert::EndId { id } => DynamicAdvert::EndId { id },
			DynamicAdvert::Update { id, hops, cost } => DynamicAdvert::Update { id, hops, cost },
		}
	}

	#[test]
	fn dynamic_advert_round_trip() {
		let mut hops = OriginList::new();
		hops.push(Origin::new(7).unwrap()).unwrap();
		hops.push(Origin::new(9).unwrap()).unwrap();

		let start = DynamicAdvert::Start {
			pattern: Pattern::new("room", "transcode.pro"),
			hops: hops.clone(),
			cost: 1000,
		};
		assert_eq!(advert_round_trip(&start, Version::Lite06Wip), start);

		// The catch-all: both halves empty.
		let any = DynamicAdvert::Start {
			pattern: Pattern::any(),
			hops: OriginList::new(),
			cost: crate::broadcast::MAX_COST,
		};
		assert_eq!(advert_round_trip(&any, Version::Lite06Wip), any);

		let end = DynamicAdvert::EndId { id: 3 };
		assert_eq!(advert_round_trip(&end, Version::Lite06Wip), end);

		let update = DynamicAdvert::Update { id: 3, hops, cost: 5 };
		assert_eq!(advert_round_trip(&update, Version::Lite06Wip), update);
	}

	#[test]
	fn dynamic_advert_rejects_older_versions() {
		let msg = DynamicAdvert::EndId { id: 1 };
		let mut buf = bytes::BytesMut::new();
		assert!(matches!(
			msg.encode(&mut buf, Version::Lite05),
			Err(EncodeError::Version)
		));

		msg.encode(&mut buf, Version::Lite06Wip).unwrap();
		let mut slice = &buf[..];
		assert!(DynamicAdvert::decode(&mut slice, Version::Lite05).is_err());
	}

	#[test]
	fn end_by_id_is_three_bytes() {
		// Type varint + length varint + id varint.
		let msg = DynamicAdvert::EndId { id: 3 };
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, Version::Lite06Wip).unwrap();
		assert_eq!(buf.len(), 3);
	}

	#[test]
	fn request_and_ok_round_trip() {
		let req = DynamicRequest {
			prefix: Path::new("room"),
		};
		let mut buf = bytes::BytesMut::new();
		req.encode(&mut buf, Version::Lite06Wip).unwrap();
		let mut slice = &buf[..];
		let got = DynamicRequest::decode(&mut slice, Version::Lite06Wip).unwrap();
		assert!(slice.is_empty());
		assert_eq!(got.prefix.as_str(), "room");

		let ok = DynamicOk {
			origin: Origin::new(42).unwrap(),
			active: 2,
		};
		let mut buf = bytes::BytesMut::new();
		ok.encode(&mut buf, Version::Lite06Wip).unwrap();
		let mut slice = &buf[..];
		let got = DynamicOk::decode(&mut slice, Version::Lite06Wip).unwrap();
		assert!(slice.is_empty());
		assert_eq!(got, ok);
	}
}
