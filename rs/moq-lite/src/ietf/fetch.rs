use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode},
	ietf::{
		GroupOrder, Location, Message, Parameters, RequestId, Version,
		namespace::{decode_namespace, encode_namespace},
	},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchType<'a> {
	//
	Standalone {
		namespace: Path<'a>,
		track: Cow<'a, str>,
		start: Location,
		end: Location,
	},
	RelativeJoining {
		subscriber_request_id: RequestId,
		group_offset: u64,
	},
	AbsoluteJoining {
		subscriber_request_id: RequestId,
		group_id: u64,
	},
}

impl<V: Copy> Encode<V> for FetchType<'_> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		match self {
			FetchType::Standalone {
				namespace,
				track,
				start,
				end,
			} => {
				1u8.encode(w, version);
				encode_namespace(w, namespace, version);
				track.encode(w, version);
				start.encode(w, version);
				end.encode(w, version);
			}
			FetchType::RelativeJoining {
				subscriber_request_id,
				group_offset,
			} => {
				2u8.encode(w, version);
				subscriber_request_id.encode(w, version);
				group_offset.encode(w, version);
			}
			FetchType::AbsoluteJoining {
				subscriber_request_id,
				group_id,
			} => {
				3u8.encode(w, version);
				subscriber_request_id.encode(w, version);
				group_id.encode(w, version);
			}
		}
	}
}

impl<V: Copy> Decode<V> for FetchType<'_> {
	fn decode<B: bytes::Buf>(buf: &mut B, version: V) -> Result<Self, DecodeError> {
		let fetch_type = u64::decode(buf, version)?;
		Ok(match fetch_type {
			0x1 => {
				let namespace = decode_namespace(buf, version)?;
				let track = Cow::<str>::decode(buf, version)?;
				let start = Location::decode(buf, version)?;
				let end = Location::decode(buf, version)?;
				FetchType::Standalone {
					namespace,
					track,
					start,
					end,
				}
			}
			0x2 => {
				let subscriber_request_id = RequestId::decode(buf, version)?;
				let group_offset = u64::decode(buf, version)?;
				FetchType::RelativeJoining {
					subscriber_request_id,
					group_offset,
				}
			}
			0x3 => {
				let subscriber_request_id = RequestId::decode(buf, version)?;
				let group_id = u64::decode(buf, version)?;
				FetchType::AbsoluteJoining {
					subscriber_request_id,
					group_id,
				}
			}
			_ => return Err(DecodeError::InvalidValue),
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch<'a> {
	pub request_id: RequestId,
	pub subscriber_priority: u8,
	pub group_order: GroupOrder,
	pub fetch_type: FetchType<'a>,
	// fetch type specific
	// parameters
}

impl Message for Fetch<'_> {
	const ID: u64 = 0x16;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.subscriber_priority.encode(w, version);
		self.group_order.encode(w, version);
		self.fetch_type.encode(w, version);
		// parameters
		0u8.encode(w, version);
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		let subscriber_priority = u8::decode(buf, version)?;
		let group_order = GroupOrder::decode(buf, version)?;
		let fetch_type = FetchType::decode(buf, version)?;
		// parameters
		let _params = Parameters::decode(buf, version)?;
		Ok(Self {
			request_id,
			subscriber_priority,
			group_order,
			fetch_type,
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOk {
	pub request_id: RequestId,
	pub group_order: GroupOrder,
	pub end_of_track: bool,
	pub end_location: Location,
	// parameters
}
impl Message for FetchOk {
	const ID: u64 = 0x18;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.group_order.encode(w, version);
		self.end_of_track.encode(w, version);
		self.end_location.encode(w, version);
		// parameters
		0u8.encode(w, version);
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		let group_order = GroupOrder::decode(buf, version)?;
		let end_of_track = bool::decode(buf, version)?;
		let end_location = Location::decode(buf, version)?;
		// parameters
		let _params = Parameters::decode(buf, version)?;
		Ok(Self {
			request_id,
			group_order,
			end_of_track,
			end_location,
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError<'a> {
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for FetchError<'_> {
	const ID: u64 = 0x19;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.error_code.encode(w, version);
		self.reason_phrase.encode(w, version);
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		let error_code = u64::decode(buf, version)?;
		let reason_phrase = Cow::<str>::decode(buf, version)?;
		Ok(Self {
			request_id,
			error_code,
			reason_phrase,
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchCancel {
	pub request_id: RequestId,
}
impl Message for FetchCancel {
	const ID: u64 = 0x17;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		Ok(Self { request_id })
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchHeader {
	pub request_id: RequestId,
}

impl FetchHeader {
	pub const TYPE: u64 = 0x5;
}

impl<V> Encode<V> for FetchHeader {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.request_id.encode(w, version);
	}
}

impl<V> Decode<V> for FetchHeader {
	fn decode<B: bytes::Buf>(buf: &mut B, version: V) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		Ok(Self { request_id })
	}
}

// Currently unused.
pub struct FetchObject {
	/*
	Group ID (i),
	Subgroup ID (i),
	Object ID (i),
	Publisher Priority (8),
	Extension Headers Length (i),
	[Extension headers (...)],
	Object Payload Length (i),
	[Object Status (i)],
	Object Payload (..),
	*/
}
