//! IETF moq-transport track status messages (v14 + v15)

use std::borrow::Cow;

use crate::{
	Path,
	coding::*,
	ietf::{Filter, GroupOrder, RequestId},
};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

/// TRACK_STATUS_OK (0x0e), the draft-14 answer to a successful TRACK_STATUS.
///
/// The body is byte-identical to SUBSCRIBE_OK, with Track Alias 0, so [`super::SubscribeOk`]
/// encodes it and only the type differs. Draft-15 and later answer with REQUEST_OK instead,
/// which is why 0x0e is free to mean NAMESPACE_DONE from draft-16 on.
pub const TRACK_STATUS_OK_ID: u64 = 0x0e;

/// TRACK_STATUS_ERROR (0x0f), the draft-14 refusal of a TRACK_STATUS.
///
/// The body is byte-identical to SUBSCRIBE_ERROR. Draft-15 and later refuse with
/// REQUEST_ERROR instead.
pub const TRACK_STATUS_ERROR_ID: u64 = 0x0f;

/// TrackStatus message (0x0d)
///
/// The format is identical to SUBSCRIBE on every draft: subscribe fields inline on v14,
/// parameters from v15 on. The publisher answers as if it were a SUBSCRIBE that creates no
/// subscription state and delivers no objects.
#[derive(Clone, Debug)]
pub struct TrackStatus<'a> {
	pub request_id: RequestId,
	pub track_namespace: Path<'a>,
	pub track_name: Cow<'a, str>,
}

impl Message for TrackStatus<'_> {
	const ID: u64 = 0x0d;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			0u64.encode(w, version)?; // required_request_id_delta = 0
		}
		encode_namespace(w, &self.track_namespace, version)?;
		self.track_name.encode(w, version)?;

		match version {
			Version::Draft14 => {
				0u8.encode(w, version)?; // subscriber priority
				GroupOrder::Descending.encode(w, version)?;
				false.encode(w, version)?; // forward
				Filter::NextObject.encode(w, version)?; // filter
				encode_params!(w, version,);
			}
			_ => {
				encode_params!(w, version,);
			}
		}
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = u64::decode(r, version)?;
		}
		let track_namespace = decode_namespace(r, version)?;
		let track_name = Cow::<str>::decode(r, version)?;

		match version {
			Version::Draft14 => {
				let _subscriber_priority = u8::decode(r, version)?;
				let _group_order = GroupOrder::decode(r, version)?;
				let _forward = bool::decode(r, version)?;
				// The whole filter, not just its tag: an absolute filter carries a Start
				// Location and an End Group after it, and skipping those desyncs the
				// parameters that follow.
				let _filter = Filter::decode(r, version)?;
				decode_params!(r, version,);
			}
			_ => {
				decode_params!(r, version,);
			}
		}

		Ok(Self {
			request_id,
			track_namespace,
			track_name,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version).unwrap();
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, version)
	}

	#[test]
	fn test_track_status_v14_round_trip() {
		let msg = TrackStatus {
			request_id: RequestId(1),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: TrackStatus = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
	}

	#[test]
	fn test_track_status_v15_round_trip() {
		let msg = TrackStatus {
			request_id: RequestId(1),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: TrackStatus = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
	}

	#[test]
	fn test_track_status_v17_round_trip() {
		let msg = TrackStatus {
			request_id: RequestId(1),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: TrackStatus = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
	}

	#[test]
	fn test_track_status_v16_round_trip() {
		let msg = TrackStatus {
			request_id: RequestId(1),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
		};

		let encoded = encode_message(&msg, Version::Draft16);
		let decoded: TrackStatus = decode_message(&encoded, Version::Draft16).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
	}

	/// The draft-14 format is SUBSCRIBE's, so a subscribe-shaped request has to decode as one.
	/// An absolute filter carries a Start Location and an End Group after its tag, and reading
	/// only the tag used to desync the parameters that follow.
	#[test]
	fn test_track_status_v14_decodes_an_absolute_filter() {
		let msg = crate::ietf::Subscribe {
			request_id: RequestId(3),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::Absolute {
				start: crate::ietf::Location { group: 4, object: 5 },
				end: Some(crate::ietf::EndLocation { group: 9, object: None }),
			},
			fill: None,
			properties_wanted: true,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: TrackStatus = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(3));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
	}

	#[test]
	fn test_track_status_v18_round_trip() {
		let msg = TrackStatus {
			request_id: RequestId(1),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: TrackStatus = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
	}
}
