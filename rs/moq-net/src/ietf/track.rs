//! IETF moq-transport track status messages (v14 + v15)

use std::borrow::Cow;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Path,
	coding::*,
	ietf::{Filter, GroupOrder, RequestId, Subscribe},
};

use super::Message;
use super::namespace::encode_namespace;

use super::Version;

/// TrackStatus message (0x0d)
/// v14: own format (TrackStatusRequest-like with subscribe fields)
/// v15: same wire format as SUBSCRIBE. Response is REQUEST_OK.
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
				0u8.encode(w, version)?; // no parameters
			}
			_ => {
				encode_params!(w, version,);
			}
		}
		Ok(())
	}

	/// Every draft defines TRACK_STATUS as identical to SUBSCRIBE, so it decodes as one and
	/// keeps only what names the track. We refuse the request, so the rest goes unread.
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let subscribe = Subscribe::decode_msg(r, version)?;
		Ok(Self {
			request_id: subscribe.request_id,
			track_namespace: subscribe.track_namespace,
			track_name: subscribe.track_name,
		})
	}
}

#[derive(Clone, Copy, Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum TrackStatusCode {
	InProgress = 0x00,
	NotFound = 0x01,
	NotAuthorized = 0x02,
	Ended = 0x03,
}

impl Encode<Version> for TrackStatusCode {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		u64::from(*self).encode(w, version)?;
		Ok(())
	}
}

impl Decode<Version> for TrackStatusCode {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Self::try_from(u64::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)
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

	/// TRACK_STATUS is identical to SUBSCRIBE on every draft, so it carries whatever
	/// fields and parameters a SUBSCRIBE can. A peer that sends them must still reach
	/// our refusal rather than having its session closed.
	#[test]
	fn test_track_status_carries_subscribe_fields() {
		// Request ID 1, Track Namespace ("live"), Track Name ("video").
		let head: &[u8] = &[
			0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x05, b'v', b'i', b'd', b'e', b'o',
		];

		#[rustfmt::skip]
		let cases: [(Version, &[u8]); 3] = [
			(Version::Draft14, &[
				0x80, // Subscriber Priority
				0x02, // Group Order
				0x00, // Forward
				0x03, 0x05, 0x01, // Filter Type AbsoluteStart, at {5, 1}
				0x00, // Number of Parameters
			]),
			(Version::Draft15, &[
				0x02, // Number of Parameters
				0x10, 0x00, // FORWARD = 0
				0x20, 0x01, // SUBSCRIBER_PRIORITY = 1
			]),
			(Version::Draft20, &[
				0x02, // Number of Parameters
				0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
				0x32, 0x00, // INCLUDE_PROPERTIES (0x35) = 0
			]),
		];

		for (version, rest) in cases {
			let body = [head, rest].concat();
			let mut buf = bytes::Bytes::from(body);
			let msg = TrackStatus::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
			assert!(buf.is_empty(), "{version}: trailing bytes");
			assert_eq!(msg.track_name, "video", "{version}");
		}
	}
}
