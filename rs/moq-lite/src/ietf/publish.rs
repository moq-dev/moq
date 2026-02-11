/*
9.13. PUBLISH
The publisher sends the PUBLISH control message to initiate a subscription to a track. The receiver verifies the publisher is authorized to publish this track.

PUBLISH Message {
  Type (i) = 0x1D,
  Length (i),
  Request ID (i),
  Track Namespace (tuple),
  Track Name Length (i),
  Track Name (..),
  Track Alias (i),
  Group Order (8),
  Content Exists (8),
  [Largest Location (Location),]
  Forward (8),
  Number of Parameters (i),
  Parameters (..) ...,
}
Figure 15: MOQT PUBLISH Message
Request ID: See Section 9.1.

Track Namespace: Identifies a track's namespace as defined in (Section 2.4.1)

Track Name: Identifies the track name as defined in (Section 2.4.1).

Track Alias: The identifer used for this track in Subgroups or Datagrams (see Section 10.1). The same Track Alias MUST NOT be used to refer to two different Tracks simultaneously. If a subscriber receives a PUBLISH that uses the same Track Alias as a different track with an active subscription, it MUST close the session with error DUPLICATE_TRACK_ALIAS.

Group Order: Indicates the subscription will be delivered in Ascending (0x1) or Descending (0x2) order by group. See Section 7. Values of 0x0 and those larger than 0x2 are a protocol error.

Content Exists: 1 if an object has been published on this track, 0 if not. If 0, then the Largest Group ID and Largest Object ID fields will not be present. Any other value is a protocol error and MUST terminate the session with a PROTOCOL_VIOLATION (Section 3.4).

Largest Location: The location of the largest object available for this track.

Forward: The forward mode for this subscription. Any value other than 0 or 1 is a PROTOCOL_VIOLATION. 0 indicates the publisher will not transmit any objects until the subscriber sets the Forward State to 1. 1 indicates the publisher will start transmitting objects immediately, even before PUBLISH_OK.

Parameters: The parameters are defined in Section 9.2.1.

A subscriber receiving a PUBLISH for a Track it does not wish to receive SHOULD send PUBLISH_ERROR with error code UNINTERESTED, and abandon reading any publisher initiated streams associated with that subscription using a STOP_SENDING frame.

9.14. PUBLISH_OK
The subscriber sends a PUBLISH_OK control message to acknowledge the successful authorization and acceptance of a PUBLISH message, and establish a subscription.

PUBLISH_OK Message {
  Type (i) = 0x1E,
  Length (i),
  Request ID (i),
  Forward (8),
  Subscriber Priority (8),
  Group Order (8),
  Filter Type (i),
  [Start Location (Location)],
  [End Group (i)],
  Number of Parameters (i),
  Parameters (..) ...,
}
Figure 16: MOQT PUBLISH_OK Message
Request ID: The Request ID of the PUBLISH this message is replying to Section 9.13.

Forward: The Forward State for this subscription, either 0 (don't forward) or 1 (forward).

Subscriber Priority: The Subscriber Priority for this subscription.

Group Order: Indicates the subscription will be delivered in Ascending (0x1) or Descending (0x2) order by group. See Section 7. Values of 0x0 and those larger than 0x2 are a protocol error. This overwrites the GroupOrder specified PUBLISH.

Filter Type, Start Location, End Group: See Section 9.7.

Parameters: Parameters associated with this message.

9.15. PUBLISH_ERROR
The subscriber sends a PUBLISH_ERROR control message to reject a subscription initiated by PUBLISH.

PUBLISH_ERROR Message {
  Type (i) = 0x1F,
  Length (i),
  Request ID (i),
  Error Code (i),
  Error Reason (Reason Phrase),
}
Figure 17: MOQT PUBLISH_ERROR Message
Request ID: The Request ID of the PUBLISH this message is replying to Section 9.13.

Error Code: Identifies an integer error code for failure.

Error Reason: Provides the reason for subscription error. See Section 1.4.3.

The application SHOULD use a relevant error code in PUBLISH_ERROR, as defined below:

INTERNAL_ERROR (0x0):
An implementation specific or generic error occurred.

UNAUTHORIZED (0x1):
The publisher is not authorized to publish the given namespace or track.

TIMEOUT (0x2):
The subscription could not be established before an implementation specific timeout.

NOT_SUPPORTED (0x3):
The endpoint does not support the PUBLISH method.

UNINTERESTED (0x4):
The namespace or track is not of interest to the endpoint.


*/

use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode},
	ietf::{
		FilterType, GroupOrder, Location, Message, MessageParameters, Parameters, RequestId, Version,
		namespace::{decode_namespace, encode_namespace},
	},
};

/// Used to be called SubscribeDone
#[derive(Clone, Debug)]
pub struct PublishDone<'a> {
	pub request_id: RequestId,
	pub status_code: u64,
	pub stream_count: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for PublishDone<'_> {
	const ID: u64 = 0x0b;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.status_code.encode(w, version);
		self.stream_count.encode(w, version);
		self.reason_phrase.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let status_code = u64::decode(r, version)?;
		let stream_count = u64::decode(r, version)?;
		let reason_phrase = Cow::<str>::decode(r, version)?;

		Ok(Self {
			request_id,
			status_code,
			stream_count,
			reason_phrase,
		})
	}
}

#[derive(Debug)]
pub struct Publish<'a> {
	pub request_id: RequestId,
	pub track_namespace: Path<'a>,
	pub track_name: Cow<'a, str>,
	pub track_alias: u64,
	pub group_order: GroupOrder,
	pub largest_location: Option<Location>,
	pub forward: bool,
	// pub parameters: Parameters,
}

impl Message for Publish<'_> {
	const ID: u64 = 0x1D;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		encode_namespace(w, &self.track_namespace, version);
		self.track_name.encode(w, version);
		self.track_alias.encode(w, version);

		match version {
			Version::Draft14 => {
				self.group_order.encode(w, version);
				if let Some(location) = &self.largest_location {
					true.encode(w, version);
					location.encode(w, version);
				} else {
					false.encode(w, version);
				}

				self.forward.encode(w, version);
				// parameters
				0u8.encode(w, version);
			}
			Version::Draft15 => {
				let mut params = MessageParameters::default();
				params.set_group_order(u8::from(self.group_order) as u64);
				if let Some(location) = &self.largest_location {
					params.set_largest_object(location);
				}
				params.set_forward(self.forward);
				params.encode(w, version);
			}
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let track_namespace = decode_namespace(r, version)?;
		let track_name = Cow::<str>::decode(r, version)?;
		let track_alias = u64::decode(r, version)?;

		match version {
			Version::Draft14 => {
				let group_order = GroupOrder::decode(r, version)?;
				let content_exists = bool::decode(r, version)?;
				let largest_location = match content_exists {
					true => Some(Location::decode(r, version)?),
					false => None,
				};
				let forward = bool::decode(r, version)?;
				// parameters
				let _params = Parameters::decode(r, version)?;

				Ok(Self {
					request_id,
					track_namespace,
					track_name,
					track_alias,
					group_order,
					largest_location,
					forward,
				})
			}
			Version::Draft15 => {
				let params = MessageParameters::decode(r, version)?;

				let group_order = match params.group_order() {
					Some(v) => u8::try_from(v)
						.ok()
						.and_then(|v| GroupOrder::try_from(v).ok())
						.map(GroupOrder::any_to_descending)
						.unwrap_or(GroupOrder::Descending),
					None => GroupOrder::Descending,
				};
				let largest_location = params.largest_object();
				let forward = params.forward().unwrap_or(true);

				Ok(Self {
					request_id,
					track_namespace,
					track_name,
					track_alias,
					group_order,
					largest_location,
					forward,
				})
			}
		}
	}
}

#[derive(Debug)]
pub struct PublishOk {
	pub request_id: RequestId,
	pub forward: bool,
	pub subscriber_priority: u8,
	pub group_order: GroupOrder,
	pub filter_type: FilterType,
	// pub parameters: Parameters,
}

impl Message for PublishOk {
	const ID: u64 = 0x1E;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);

		match version {
			Version::Draft14 => {
				self.forward.encode(w, version);
				self.subscriber_priority.encode(w, version);
				self.group_order.encode(w, version);
				self.filter_type.encode(w, version);
				debug_assert!(
					matches!(self.filter_type, FilterType::LargestObject | FilterType::NextGroup),
					"absolute subscribe not supported"
				);
				// no parameters
				0u8.encode(w, version);
			}
			Version::Draft15 => {
				let mut params = MessageParameters::default();
				params.set_forward(self.forward);
				params.set_subscriber_priority(self.subscriber_priority);
				params.set_group_order(u8::from(self.group_order) as u64);
				params.set_subscription_filter(self.filter_type);
				params.encode(w, version);
			}
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;

		match version {
			Version::Draft14 => {
				let forward = bool::decode(r, version)?;
				let subscriber_priority = u8::decode(r, version)?;
				let group_order = GroupOrder::decode(r, version)?;
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

				// no parameters
				let _params = Parameters::decode(r, version)?;

				Ok(Self {
					request_id,
					forward,
					subscriber_priority,
					group_order,
					filter_type,
				})
			}
			Version::Draft15 => {
				let params = MessageParameters::decode(r, version)?;

				let forward = params.forward().unwrap_or(true);
				let subscriber_priority = params.subscriber_priority().unwrap_or(128);
				let group_order = match params.group_order() {
					Some(v) => u8::try_from(v)
						.ok()
						.and_then(|v| GroupOrder::try_from(v).ok())
						.map(GroupOrder::any_to_descending)
						.unwrap_or(GroupOrder::Descending),
					None => GroupOrder::Descending,
				};
				let filter_type = params.subscription_filter().unwrap_or(FilterType::LargestObject);

				Ok(Self {
					request_id,
					forward,
					subscriber_priority,
					group_order,
					filter_type,
				})
			}
		}
	}
}

#[derive(Debug)]
pub struct PublishError<'a> {
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}
impl Message for PublishError<'_> {
	const ID: u64 = 0x1F;

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
	fn test_publish_v14_round_trip() {
		let msg = Publish {
			request_id: RequestId(1),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
			track_alias: 42,
			group_order: GroupOrder::Descending,
			largest_location: Some(Location { group: 10, object: 5 }),
			forward: true,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: Publish = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.track_alias, 42);
		assert_eq!(decoded.largest_location, Some(Location { group: 10, object: 5 }));
		assert!(decoded.forward);
	}

	#[test]
	fn test_publish_v15_round_trip() {
		let msg = Publish {
			request_id: RequestId(1),
			track_namespace: Path::new("test/ns"),
			track_name: "video".into(),
			track_alias: 42,
			group_order: GroupOrder::Descending,
			largest_location: Some(Location { group: 10, object: 5 }),
			forward: true,
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: Publish = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.track_alias, 42);
		assert_eq!(decoded.largest_location, Some(Location { group: 10, object: 5 }));
		assert!(decoded.forward);
	}

	#[test]
	fn test_publish_ok_v14_round_trip() {
		let msg = PublishOk {
			request_id: RequestId(7),
			forward: true,
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter_type: FilterType::LargestObject,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: PublishOk = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(7));
		assert!(decoded.forward);
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_publish_ok_v15_round_trip() {
		let msg = PublishOk {
			request_id: RequestId(7),
			forward: true,
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter_type: FilterType::LargestObject,
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: PublishOk = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(7));
		assert!(decoded.forward);
		assert_eq!(decoded.subscriber_priority, 128);
	}
}
