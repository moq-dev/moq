//! IETF moq-transport-14 subscribe messages

use std::borrow::Cow;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	coding::*,
	ietf::{GroupOrder, Location, Message, Parameters, RequestId, Version},
	Path, Time,
};

use super::namespace::{decode_namespace, encode_namespace};

#[derive(Debug, Copy, Clone, IntoPrimitive, Eq, Hash, PartialEq)]
#[repr(u64)]
pub enum SubscribeParameter {
	DeliveryTimeout = 0x2,
	MaxCacheDuration = 0x4,
}

#[derive(Clone, Copy, Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u64)]
pub enum FilterType {
	NextGroup = 0x01,
	LargestObject = 0x2,
	AbsoluteStart = 0x3,
	AbsoluteRange = 0x4,
}

impl<V> Encode<V> for FilterType {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		u64::from(*self).encode(w, version);
	}
}

impl<V> Decode<V> for FilterType {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
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
	pub delivery_timeout: Time,
}

impl<'a> Message for Subscribe<'a> {
	const ID: u64 = 0x03;

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;

		// Decode namespace (tuple of strings)
		let track_namespace = decode_namespace(r, version)?;

		let track_name = Cow::<str>::decode(r, version)?;
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

		let params = Parameters::decode(r, version)?;
		let delivery_timeout = params.get_int(SubscribeParameter::DeliveryTimeout.into()).unwrap_or(0);
		let delivery_timeout = Time::from_millis(delivery_timeout).map_err(|_| DecodeError::InvalidValue)?;

		Ok(Self {
			request_id,
			track_namespace,
			track_name,
			subscriber_priority,
			delivery_timeout,
			group_order,
			filter_type,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		encode_namespace(w, &self.track_namespace, version);
		self.track_name.encode(w, version);
		self.subscriber_priority.encode(w, version);
		GroupOrder::Descending.encode(w, version);
		true.encode(w, version); // forward

		assert!(
			!matches!(self.filter_type, FilterType::AbsoluteStart | FilterType::AbsoluteRange),
			"Absolute subscribe not supported"
		);

		self.filter_type.encode(w, version);

		let mut params = Parameters::default();
		params.set_int(
			SubscribeParameter::DeliveryTimeout.into(),
			self.delivery_timeout.as_millis() as u64,
		);
		params.encode(w, version);
	}
}

/// SubscribeOk message (0x04)
#[derive(Clone, Debug)]
pub struct SubscribeOk {
	pub request_id: RequestId,
	pub track_alias: u64,
	pub delivery_timeout: Time,
	pub group_order: GroupOrder,
}

impl Message for SubscribeOk {
	const ID: u64 = 0x04;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.track_alias.encode(w, version);
		0u8.encode(w, version); // subscription expires
		self.group_order.encode(w, version);
		false.encode(w, version); // no content

		let mut params = Parameters::default();
		params.set_int(
			SubscribeParameter::DeliveryTimeout.into(),
			self.delivery_timeout.as_millis() as u64,
		);
		params.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let track_alias = u64::decode(r, version)?;

		let expires = u64::decode(r, version)?;
		if expires != 0 {
			return Err(DecodeError::Unsupported);
		}

		let group_order = GroupOrder::decode(r, version)?;

		// TODO: We don't support largest group/object yet
		if bool::decode(r, version)? {
			let _group = u64::decode(r, version)?;
			let _object = u64::decode(r, version)?;
		}

		let params = Parameters::decode(r, version)?;
		let delivery_timeout = params.get_int(SubscribeParameter::DeliveryTimeout.into()).unwrap_or(0);
		let delivery_timeout = Time::from_millis(delivery_timeout).map_err(|_| DecodeError::InvalidValue)?;

		Ok(Self {
			request_id,
			track_alias,
			delivery_timeout,
			group_order,
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

impl<'a> Message for SubscribeError<'a> {
	const ID: u64 = 0x05;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.error_code.encode(w, version);
		self.reason_phrase.encode(w, version);
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

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

/*
  Type (i) = 0x2,
  Length (16),
  Request ID (i),
  Subscription Request ID (i),
  Start Location (Location),
  End Group (i),
  Subscriber Priority (8),
  Forward (8),
  Number of Parameters (i),
  Parameters (..) ...
*/
#[derive(Debug)]
pub struct SubscribeUpdate {
	pub request_id: RequestId,
	pub subscription_request_id: RequestId,
	pub start_location: Location,
	pub end_group: u64,
	pub subscriber_priority: u8,
	pub delivery_timeout: Time,
}

impl Message for SubscribeUpdate {
	const ID: u64 = 0x02;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.subscription_request_id.encode(w, version);
		self.start_location.encode(w, version);
		self.end_group.encode(w, version);
		self.subscriber_priority.encode(w, version);
		true.encode(w, version); // forward

		let mut params = Parameters::default();
		params.set_int(
			SubscribeParameter::DeliveryTimeout.into(),
			self.delivery_timeout.as_millis() as u64,
		);
		params.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let subscription_request_id = RequestId::decode(r, version)?;
		let start_location = Location::decode(r, version)?;
		let end_group = u64::decode(r, version)?;
		let subscriber_priority = u8::decode(r, version)?;
		let forward = bool::decode(r, version)?;
		if !forward {
			return Err(DecodeError::Unsupported);
		}

		let parameters = Parameters::decode(r, version)?;
		let delivery_timeout = parameters
			.get_int(SubscribeParameter::DeliveryTimeout.into())
			.unwrap_or(0);
		let delivery_timeout = Time::from_millis(delivery_timeout).map_err(|_| DecodeError::InvalidValue)?;

		Ok(Self {
			request_id,
			subscription_request_id,
			start_location,
			end_group,
			subscriber_priority,
			delivery_timeout,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Draft14);
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8]) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, Version::Draft14)
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
			delivery_timeout: Time::from_millis(1000).unwrap(),
		};

		let encoded = encode_message(&msg);
		let decoded: Subscribe = decode_message(&encoded).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.subscriber_priority, 128);
		assert_eq!(decoded.delivery_timeout, Time::from_millis(1000).unwrap());
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
			delivery_timeout: Time::from_millis(1000).unwrap(),
		};

		let encoded = encode_message(&msg);
		let decoded: Subscribe = decode_message(&encoded).unwrap();

		assert_eq!(decoded.track_namespace.as_str(), "conference/room123");
		assert_eq!(decoded.delivery_timeout, Time::from_millis(1000).unwrap());
	}

	#[test]
	fn test_subscribe_ok() {
		let msg = SubscribeOk {
			request_id: RequestId(42),
			track_alias: 42,
			delivery_timeout: Time::from_millis(1000).unwrap(),
			group_order: GroupOrder::Ascending,
		};

		let encoded = encode_message(&msg);
		let decoded: SubscribeOk = decode_message(&encoded).unwrap();

		assert_eq!(decoded.request_id, RequestId(42));
		assert_eq!(decoded.track_alias, 42);
		assert_eq!(decoded.delivery_timeout, Time::from_millis(1000).unwrap());
		assert_eq!(decoded.group_order, GroupOrder::Ascending);
	}

	#[test]
	fn test_subscribe_error() {
		let msg = SubscribeError {
			request_id: RequestId(123),
			error_code: 500,
			reason_phrase: "Not found".into(),
		};

		let encoded = encode_message(&msg);
		let decoded: SubscribeError = decode_message(&encoded).unwrap();

		assert_eq!(decoded.request_id, RequestId(123));
		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Not found");
	}

	#[test]
	fn test_unsubscribe() {
		let msg = Unsubscribe {
			request_id: RequestId(999),
		};

		let encoded = encode_message(&msg);
		let decoded: Unsubscribe = decode_message(&encoded).unwrap();

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

		let result: Result<Subscribe, _> = decode_message(&invalid_bytes);
		assert!(result.is_err());
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

		let result: Result<SubscribeOk, _> = decode_message(&invalid_bytes);
		assert!(result.is_err());
	}
}
