//! IETF moq-transport track status messages (v14 + v15)

use std::borrow::Cow;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Path,
	coding::*,
	ietf::{Filter, GroupOrder, Parameters, RequestId},
};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};

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

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			w.varint(0)?; // required_request_id_delta = 0
		}
		encode_namespace(w, &self.track_namespace)?;
		w.string(&self.track_name)?;

		match version {
			Version::Draft14 => {
				w.u8(0); // subscriber priority
				GroupOrder::Descending.encode(w, version)?;
				w.bool(false); // forward
				Filter::NextObject.encode(w, version)?; // filter
				w.u8(0); // no parameters
			}
			_ => {
				encode_params!(w, version,);
			}
		}
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = r.varint()?;
		}
		let track_namespace = decode_namespace(r)?;
		let track_name = Cow::Owned(r.string()?);

		match version {
			Version::Draft14 => {
				let _subscriber_priority = r.u8()?;
				let _group_order = GroupOrder::decode(r, version)?;
				let _forward = r.bool()?;
				let _filter_type = r.varint()?;
				let _params = Parameters::decode(r, version)?;
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

#[derive(Clone, Copy, Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum TrackStatusCode {
	InProgress = 0x00,
	NotFound = 0x01,
	NotAuthorized = 0x02,
	Ended = 0x03,
}

impl Encode<Version> for TrackStatusCode {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(u64::from(*self))?;
		Ok(())
	}
}

impl Decode<Version> for TrackStatusCode {
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		Self::try_from(r.varint()?).map_err(|_| DecodeError::InvalidValue)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn encode_message<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		crate::coding::decode_buf(&mut buf, version, M::decode_msg)
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
}
