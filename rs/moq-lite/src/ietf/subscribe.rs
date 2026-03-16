//! IETF moq-transport subscribe messages (v14 + v15)

use std::borrow::Cow;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Path,
	coding::*,
	ietf::{GroupOrder, Location, Parameters, RequestId},
};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum FilterType {
	NextGroup = 0x01,
	#[default]
	LargestObject = 0x2,
	AbsoluteStart = 0x3,
	AbsoluteRange = 0x4,
}

impl Encode<Version> for FilterType {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		u64::from(*self).encode(w, version)?;
		Ok(())
	}
}

impl Decode<Version> for FilterType {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Self::try_from(u64::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)
	}
}

/// Subscribe message (0x03)
/// Sent by the subscriber to request all future objects for the given track.
#[derive(Clone, Debug)]
pub struct Subscribe<'a> {
	pub request_id: RequestId,
	pub track_namespace: Path<'a>,
	pub track_name: Cow<'a, str>,
	pub subscriber_priority: u8,
	pub group_order: GroupOrder,
	pub filter_type: FilterType,
}

impl Message for Subscribe<'_> {
	const ID: u64 = 0x03;

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = u64::decode(r, version)?;
		}
		let track_namespace = decode_namespace(r, version)?;
		let track_name = Cow::<str>::decode(r, version)?;

		match version {
			Version::Draft14 => {
				let subscriber_priority = u8::decode(r, version)?;
				let group_order = GroupOrder::decode(r, version)?;

				let forward = bool::decode(r, version)?;
				if !forward {
					return Err(DecodeError::Unsupported);
				}

				let filter_type = FilterType::decode(r, version)?;
				match filter_type {
					FilterType::AbsoluteStart => {
						let _start = Location::decode(r, version)?;
					}
					FilterType::AbsoluteRange => {
						let _start = Location::decode(r, version)?;
						let _end_group = u64::decode(r, version)?;
					}
					FilterType::NextGroup | FilterType::LargestObject => {}
				};

				let _params = Parameters::decode(r, version)?;

				Ok(Self {
					request_id,
					track_namespace,
					track_name,
					subscriber_priority,
					group_order,
					filter_type,
				})
			}
			Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				decode_params!(r, version,
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => filter_type: Option<FilterType>,
					0x22 => group_order: Option<GroupOrder>,
				);

				if forward == Some(false) {
					return Err(DecodeError::Unsupported);
				}

				let subscriber_priority = subscriber_priority.unwrap_or(128);
				let group_order = group_order.unwrap_or(GroupOrder::Descending);
				let filter_type = filter_type.unwrap_or(FilterType::LargestObject);

				Ok(Self {
					request_id,
					track_namespace,
					track_name,
					subscriber_priority,
					group_order,
					filter_type,
				})
			}
		}
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			0u64.encode(w, version)?; // required_request_id_delta = 0
		}
		encode_namespace(w, &self.track_namespace, version)?;
		self.track_name.encode(w, version)?;

		match version {
			Version::Draft14 => {
				self.subscriber_priority.encode(w, version)?;
				self.group_order.encode(w, version)?;
				true.encode(w, version)?; // forward

				debug_assert!(
					!matches!(self.filter_type, FilterType::AbsoluteStart | FilterType::AbsoluteRange),
					"Absolute subscribe not supported"
				);

				self.filter_type.encode(w, version)?;
				0u8.encode(w, version)?; // no parameters
			}
			Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				encode_params!(w, version,
					0x10 => true,
					0x20 => self.subscriber_priority,
					0x21 => self.filter_type,
					0x22 => self.group_order,
				);
			}
		}

		Ok(())
	}
}

/// SubscribeOk message (0x04)
#[derive(Clone, Debug)]
pub struct SubscribeOk {
	pub request_id: Option<RequestId>,
	pub track_alias: u64,
}

impl Message for SubscribeOk {
	const ID: u64 = 0x04;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if version != Version::Draft17 {
			self.request_id
				.expect("request_id required for draft14-16")
				.encode(w, version)?;
		} else {
			assert!(self.request_id.is_none(), "request_id must be None for draft17");
		}
		self.track_alias.encode(w, version)?;

		match version {
			Version::Draft14 => {
				0u64.encode(w, version)?; // expires = 0
				GroupOrder::Descending.encode(w, version)?;
				false.encode(w, version)?; // no content
				0u8.encode(w, version)?; // no parameters
			}
			Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				encode_params!(w, version,
					0x22 => GroupOrder::Descending,
				);
			}
		}

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = if version == Version::Draft17 {
			None
		} else {
			Some(RequestId::decode(r, version)?)
		};
		let track_alias = u64::decode(r, version)?;

		match version {
			Version::Draft14 => {
				let expires = u64::decode(r, version)?;
				if expires != 0 {
					return Err(DecodeError::Unsupported);
				}

				let _group_order = u8::decode(r, version)?;

				if bool::decode(r, version)? {
					let _group = u64::decode(r, version)?;
					let _object = u64::decode(r, version)?;
				}

				let _params = Parameters::decode(r, version)?;
			}
			Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				decode_params!(r, version,
					0x22 => _group_order: Option<GroupOrder>,
				);
				super::properties::skip_properties(r, version)?;
			}
		}

		Ok(Self {
			request_id,
			track_alias,
		})
	}
}

/// SubscribeError message (0x05)
#[derive(Clone, Debug)]
pub struct SubscribeError<'a> {
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for SubscribeError<'_> {
	const ID: u64 = 0x05;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		self.error_code.encode(w, version)?;
		self.reason_phrase.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let error_code = u64::decode(r, version)?;
		let reason_phrase = Cow::<str>::decode(r, version)?;

		Ok(Self {
			request_id,
			error_code,
			reason_phrase,
		})
	}
}

/// Unsubscribe message (0x0a)
#[derive(Clone, Debug)]
pub struct Unsubscribe {
	pub request_id: RequestId,
}

impl Message for Unsubscribe {
	const ID: u64 = 0x0a;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

/// SubscribeUpdate message (0x02)
#[derive(Debug)]
pub struct SubscribeUpdate {
	pub request_id: RequestId,
	pub subscription_request_id: Option<RequestId>,
	pub start_location: Location,
	pub end_group: u64,
	pub subscriber_priority: u8,
	pub forward: bool,
}

impl Message for SubscribeUpdate {
	const ID: u64 = 0x02;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft14 => {
				self.request_id.encode(w, version)?;
				self.subscription_request_id
					.expect("subscription_request_id required for draft14")
					.encode(w, version)?;
				self.start_location.encode(w, version)?;
				self.end_group.encode(w, version)?;
				self.subscriber_priority.encode(w, version)?;
				self.forward.encode(w, version)?;
				0u8.encode(w, version)?; // no parameters
			}
			Version::Draft15 | Version::Draft16 => {
				self.request_id.encode(w, version)?;
				self.subscription_request_id
					.expect("subscription_request_id required for draft15-16")
					.encode(w, version)?;
				encode_params!(w, version,
					0x10 => self.forward,
					0x20 => self.subscriber_priority,
					0x21 => FilterType::LargestObject,
				);
			}
			Version::Draft17 => {
				assert!(
					self.subscription_request_id.is_none(),
					"subscription_request_id must be None for draft17"
				);
				// REQUEST_UPDATE: request_id, required_request_id_delta, params
				self.request_id.encode(w, version)?;
				0u64.encode(w, version)?; // required_request_id_delta = 0
				encode_params!(w, version,
					0x10 => self.forward,
					0x20 => self.subscriber_priority,
					0x21 => FilterType::LargestObject,
				);
			}
		}

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 => {
				let request_id = RequestId::decode(r, version)?;
				let subscription_request_id = Some(RequestId::decode(r, version)?);
				let start_location = Location::decode(r, version)?;
				let end_group = u64::decode(r, version)?;
				let subscriber_priority = u8::decode(r, version)?;
				let forward = bool::decode(r, version)?;
				let _parameters = Parameters::decode(r, version)?;

				Ok(Self {
					request_id,
					subscription_request_id,
					start_location,
					end_group,
					subscriber_priority,
					forward,
				})
			}
			Version::Draft15 | Version::Draft16 => {
				let request_id = RequestId::decode(r, version)?;
				let subscription_request_id = Some(RequestId::decode(r, version)?);
				decode_params!(r, version,
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => _filter_type: Option<FilterType>,
				);

				let subscriber_priority = subscriber_priority.unwrap_or(128);
				let forward = forward.unwrap_or(true);

				Ok(Self {
					request_id,
					subscription_request_id,
					start_location: Location { group: 0, object: 0 },
					end_group: 0,
					subscriber_priority,
					forward,
				})
			}
			Version::Draft17 => {
				// REQUEST_UPDATE: request_id, required_request_id_delta, params
				let request_id = RequestId::decode(r, version)?;
				let _required_request_id_delta = u64::decode(r, version)?;
				decode_params!(r, version,
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => _filter_type: Option<FilterType>,
				);

				let subscriber_priority = subscriber_priority.unwrap_or(128);
				let forward = forward.unwrap_or(true);

				Ok(Self {
					request_id,
					subscription_request_id: None,
					start_location: Location { group: 0, object: 0 },
					end_group: 0,
					subscriber_priority,
					forward,
				})
			}
		}
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
	fn test_subscribe_round_trip() {
		let msg = Subscribe {
			request_id: RequestId(1),
			track_namespace: Path::new("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter_type: FilterType::LargestObject,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_subscribe_round_trip_v15() {
		let msg = Subscribe {
			request_id: RequestId(1),
			track_namespace: Path::new("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter_type: FilterType::LargestObject,
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_subscribe_nested_namespace() {
		let msg = Subscribe {
			request_id: RequestId(100),
			track_namespace: Path::new("conference/room123"),
			track_name: "audio".into(),
			subscriber_priority: 255,
			group_order: GroupOrder::Descending,
			filter_type: FilterType::LargestObject,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.track_namespace.as_str(), "conference/room123");
	}

	#[test]
	fn test_subscribe_ok() {
		let msg = SubscribeOk {
			request_id: Some(RequestId(42)),
			track_alias: 42,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(42)));
	}

	#[test]
	fn test_subscribe_ok_v15() {
		let msg = SubscribeOk {
			request_id: Some(RequestId(42)),
			track_alias: 42,
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(42)));
		assert_eq!(decoded.track_alias, 42);
	}

	#[test]
	fn test_subscribe_error() {
		let msg = SubscribeError {
			request_id: RequestId(123),
			error_code: 500,
			reason_phrase: "Not found".into(),
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: SubscribeError = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(123));
		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Not found");
	}

	#[test]
	fn test_unsubscribe() {
		let msg = Unsubscribe {
			request_id: RequestId(999),
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: Unsubscribe = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(999));
	}

	#[test]
	fn test_subscribe_rejects_invalid_filter_type() {
		#[rustfmt::skip]
		let invalid_bytes = vec![
			0x01, // subscribe_id
			0x02, // track_alias
			0x01, // namespace length
			0x04, 0x74, 0x65, 0x73, 0x74, // "test"
			0x05, 0x76, 0x69, 0x64, 0x65, 0x6f, // "video"
			0x80, // subscriber_priority
			0x02, // group_order
			0x99, // INVALID filter_type
			0x00, // num_params
		];

		let result: Result<Subscribe, _> = decode_message(&invalid_bytes, Version::Draft14);
		assert!(result.is_err());
	}

	#[test]
	fn test_subscribe_update_v15_round_trip() {
		let msg = SubscribeUpdate {
			request_id: RequestId(10),
			subscription_request_id: Some(RequestId(5)),
			start_location: Location { group: 0, object: 0 },
			end_group: 0,
			subscriber_priority: 200,
			forward: true,
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: SubscribeUpdate = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(10));
		assert_eq!(decoded.subscription_request_id, Some(RequestId(5)));
		assert_eq!(decoded.subscriber_priority, 200);
		assert!(decoded.forward);
	}

	#[test]
	fn test_subscribe_update_v14_round_trip() {
		let msg = SubscribeUpdate {
			request_id: RequestId(10),
			subscription_request_id: Some(RequestId(5)),
			start_location: Location { group: 1, object: 2 },
			end_group: 100,
			subscriber_priority: 200,
			forward: true,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: SubscribeUpdate = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(10));
		assert_eq!(decoded.subscription_request_id, Some(RequestId(5)));
		assert_eq!(decoded.start_location, Location { group: 1, object: 2 });
		assert_eq!(decoded.end_group, 100);
		assert_eq!(decoded.subscriber_priority, 200);
		assert!(decoded.forward);
	}

	#[test]
	fn test_subscribe_ok_rejects_non_zero_expires() {
		#[rustfmt::skip]
		let invalid_bytes = vec![
			0x01, // subscribe_id
			0x05, // INVALID: expires = 5
			0x02, // group_order
			0x00, // content_exists
			0x00, // num_params
		];

		let result: Result<SubscribeOk, _> = decode_message(&invalid_bytes, Version::Draft14);
		assert!(result.is_err());
	}

	#[test]
	fn test_subscribe_v17_round_trip() {
		let msg = Subscribe {
			request_id: RequestId(1),
			track_namespace: Path::new("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter_type: FilterType::LargestObject,
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_subscribe_ok_v17_round_trip() {
		let msg = SubscribeOk {
			request_id: None,
			track_alias: 42,
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, None);
		assert_eq!(decoded.track_alias, 42);
	}

	#[test]
	fn test_subscribe_update_v17_round_trip() {
		let msg = SubscribeUpdate {
			request_id: RequestId(10),
			subscription_request_id: None,
			start_location: Location { group: 0, object: 0 },
			end_group: 0,
			subscriber_priority: 200,
			forward: true,
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: SubscribeUpdate = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, RequestId(10));
		assert_eq!(decoded.subscription_request_id, None);
		assert_eq!(decoded.subscriber_priority, 200);
		assert!(decoded.forward);
	}
}
