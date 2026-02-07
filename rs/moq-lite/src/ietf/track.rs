//! IETF moq-transport track status messages (v14 + v15)

use std::borrow::Cow;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Path,
	coding::*,
	ietf::{FilterType, GroupOrder, Message, MessageParameters, Parameters, RequestId, Version},
};

use super::namespace::{decode_namespace, encode_namespace};

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

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		encode_namespace(w, &self.track_namespace, version);
		self.track_name.encode(w, version);

		match version {
			Version::Draft14 => {
				0u8.encode(w, version); // subscriber priority
				GroupOrder::Descending.encode(w, version);
				false.encode(w, version); // forward
				FilterType::LargestObject.encode(w, version); // filter type
				0u8.encode(w, version); // no parameters
			}
			Version::Draft15 => {
				// v15: same format as Subscribe - fields in parameters
				let params = MessageParameters::default();
				params.encode(w, version);
			}
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let track_namespace = decode_namespace(r, version)?;
		let track_name = Cow::<str>::decode(r, version)?;

		match version {
			Version::Draft14 => {
				let _subscriber_priority = u8::decode(r, version)?;
				let _group_order = GroupOrder::decode(r, version)?;
				let _forward = bool::decode(r, version)?;
				let _filter_type = u64::decode(r, version)?;
				let _params = Parameters::decode(r, version)?;
			}
			Version::Draft15 => {
				let _params = MessageParameters::decode(r, version)?;
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

impl<V> Encode<V> for TrackStatusCode {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		u64::from(*self).encode(w, version);
	}
}

impl<V> Decode<V> for TrackStatusCode {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		Self::try_from(u64::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)
	}
}
