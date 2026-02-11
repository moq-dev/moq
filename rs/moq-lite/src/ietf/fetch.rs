use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode},
	ietf::{
		GroupOrder, Location, Message, MessageParameters, Parameters, RequestId, Version,
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
}

impl Message for Fetch<'_> {
	const ID: u64 = 0x16;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);

		match version {
			Version::Draft14 => {
				self.subscriber_priority.encode(w, version);
				self.group_order.encode(w, version);
				self.fetch_type.encode(w, version);
				0u8.encode(w, version); // no parameters
			}
			Version::Draft15 => {
				// v15: request_id, fetch_type, parameters (with subscriber_priority, group_order)
				self.fetch_type.encode(w, version);
				let mut params = MessageParameters::default();
				params.set_subscriber_priority(self.subscriber_priority);
				params.set_group_order(u8::from(self.group_order) as u64);
				params.encode(w, version);
			}
		}
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;

		match version {
			Version::Draft14 => {
				let subscriber_priority = u8::decode(buf, version)?;
				let group_order = GroupOrder::decode(buf, version)?;
				let fetch_type = FetchType::decode(buf, version)?;
				let _params = Parameters::decode(buf, version)?;
				Ok(Self {
					request_id,
					subscriber_priority,
					group_order,
					fetch_type,
				})
			}
			Version::Draft15 => {
				let fetch_type = FetchType::decode(buf, version)?;
				let params = MessageParameters::decode(buf, version)?;

				let subscriber_priority = params.subscriber_priority().unwrap_or(128);
				let group_order = match params.group_order() {
					Some(v) => u8::try_from(v)
						.ok()
						.and_then(|v| GroupOrder::try_from(v).ok())
						.map(GroupOrder::any_to_descending)
						.unwrap_or(GroupOrder::Descending),
					None => GroupOrder::Descending,
				};

				Ok(Self {
					request_id,
					subscriber_priority,
					group_order,
					fetch_type,
				})
			}
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOk {
	pub request_id: RequestId,
	pub group_order: GroupOrder,
	pub end_of_track: bool,
	pub end_location: Location,
}
impl Message for FetchOk {
	const ID: u64 = 0x18;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);

		match version {
			Version::Draft14 => {
				self.group_order.encode(w, version);
				self.end_of_track.encode(w, version);
				self.end_location.encode(w, version);
				0u8.encode(w, version); // no parameters
			}
			Version::Draft15 => {
				// v15: request_id, end_of_track(8), end_location, parameters
				self.end_of_track.encode(w, version);
				self.end_location.encode(w, version);
				let mut params = MessageParameters::default();
				params.set_group_order(u8::from(self.group_order) as u64);
				params.encode(w, version);
			}
		}
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;

		match version {
			Version::Draft14 => {
				let group_order = GroupOrder::decode(buf, version)?;
				let end_of_track = bool::decode(buf, version)?;
				let end_location = Location::decode(buf, version)?;
				let _params = Parameters::decode(buf, version)?;
				Ok(Self {
					request_id,
					group_order,
					end_of_track,
					end_location,
				})
			}
			Version::Draft15 => {
				let end_of_track = bool::decode(buf, version)?;
				let end_location = Location::decode(buf, version)?;
				let params = MessageParameters::decode(buf, version)?;

				let group_order = match params.group_order() {
					Some(v) => u8::try_from(v)
						.ok()
						.and_then(|v| GroupOrder::try_from(v).ok())
						.map(GroupOrder::any_to_descending)
						.unwrap_or(GroupOrder::Descending),
					None => GroupOrder::Descending,
				};

				Ok(Self {
					request_id,
					group_order,
					end_of_track,
					end_location,
				})
			}
		}
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

/// Fetch object serialization (v14 format).
/// v15 adds SerializationFlags for delta encoding but we skip that for now.
pub struct FetchObject {
	/*
	v14:
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

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version);
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, version)
	}

	#[test]
	fn test_fetch_v14_round_trip() {
		let msg = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Standalone {
				namespace: Path::new("test"),
				track: "video".into(),
				start: Location { group: 0, object: 0 },
				end: Location { group: 10, object: 5 },
			},
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: Fetch = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_fetch_v15_round_trip() {
		let msg = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Standalone {
				namespace: Path::new("test"),
				track: "video".into(),
				start: Location { group: 0, object: 0 },
				end: Location { group: 10, object: 5 },
			},
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: Fetch = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_fetch_ok_v14_round_trip() {
		let msg = FetchOk {
			request_id: RequestId(2),
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: FetchOk = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(2));
		assert!(!decoded.end_of_track);
		assert_eq!(decoded.end_location, Location { group: 5, object: 3 });
	}

	#[test]
	fn test_fetch_ok_v15_round_trip() {
		let msg = FetchOk {
			request_id: RequestId(2),
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: FetchOk = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(2));
		assert!(!decoded.end_of_track);
		assert_eq!(decoded.end_location, Location { group: 5, object: 3 });
	}
}
