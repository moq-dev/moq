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
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
	ietf::{
		Filter, GroupOrder, Location, Properties, RequestId,
		namespace::{decode_track, encode_track},
	},
};

use super::Message;

use super::Version;

/// PUBLISH_DONE statuses emitted by this implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PublishDoneStatus {
	/// An implementation-specific failure ended the subscription.
	InternalError,
	/// The subscriber is no longer authorized for the track.
	Unauthorized,
	/// The track is no longer being published.
	TrackEnded,
	/// A requested subscription update could not be applied.
	UpdateFailed,
}

impl PublishDoneStatus {
	/// Return the registered wire code. Draft-22 removed SUBSCRIPTION_ENDED (0x3), which
	/// this implementation never emitted, and left the rest of the registry alone.
	pub(crate) const fn code(self) -> u64 {
		match self {
			Self::InternalError => 0x0,
			Self::Unauthorized => 0x1,
			Self::TrackEnded => 0x2,
			Self::UpdateFailed => 0x8,
		}
	}
}

/// Used to be called SubscribeDone
#[derive(Clone, Debug)]
pub struct PublishDone<'a> {
	pub status_code: u64,
	pub stream_count: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl PublishDone<'_> {
	/// Copy the borrowed reason, so the message outlives the bytes it came from.
	pub fn into_owned(self) -> PublishDone<'static> {
		PublishDone {
			status_code: self.status_code,
			stream_count: self.stream_count,
			reason_phrase: Cow::Owned(self.reason_phrase.into_owned()),
		}
	}
}

impl PublishDone<'_> {
	/// How the publisher ended the subscription: cleanly, or with the error its status names.
	pub(crate) fn end(&self, version: Version) -> Result<(), crate::Error> {
		match self.status_code {
			code if code == PublishDoneStatus::TrackEnded.code() => Ok(()),
			code if code == PublishDoneStatus::Unauthorized.code() => Err(crate::Error::Unauthorized),
			// SUBSCRIPTION_ENDED: the subscription reached the end its filter asked for.
			// Draft-22 removed it and left 0x3 unassigned.
			0x3 if matches!(version, Version::Draft18) => Ok(()),
			code => Err(crate::Error::Remote(u32::try_from(code).unwrap_or(u32::MAX))),
		}
	}
}

impl<'a> Message<'a> for PublishDone<'a> {
	const ID: u64 = 0x0b;

	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(self.status_code)?;
		w.varint(self.stream_count)?;
		w.string(&self.reason_phrase)?;
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let status_code = r.varint()?;
		let stream_count = r.varint()?;
		let reason_phrase = Cow::Borrowed(r.str()?);

		Ok(Self {
			status_code,
			stream_count,
			reason_phrase,
		})
	}
}

#[derive(Debug)]
pub struct Publish<'a> {
	pub request_id: RequestId,
	pub track_namespace: Path,
	pub track_name: Cow<'a, str>,
	pub track_alias: u64,
	pub largest_location: Option<Location>,
	pub forward: bool,

	/// Metadata about the track, sent as Track Properties.
	pub properties: Properties,
	/// The subscription's SUBSCRIBER_PRIORITY (draft-22), `None` when absent: 128.
	pub subscriber_priority: Option<u8>,
	/// The subscription's GROUP_ORDER (draft-22), `None` when absent: the track's own.
	pub group_order: Option<GroupOrder>,
	/// The subscription's Location Filter (draft-22), `None` when absent: unfiltered.
	pub filter: Option<Filter>,
}

impl Publish<'_> {
	/// Copy the borrowed track name, so the request outlives the bytes it came from.
	pub fn into_owned(self) -> Publish<'static> {
		Publish {
			request_id: self.request_id,
			track_namespace: self.track_namespace,
			track_name: Cow::Owned(self.track_name.into_owned()),
			track_alias: self.track_alias,
			largest_location: self.largest_location,
			forward: self.forward,
			properties: self.properties,
			subscriber_priority: self.subscriber_priority,
			group_order: self.group_order,
			filter: self.filter,
		}
	}
}

impl<'a> Message<'a> for Publish<'a> {
	const ID: u64 = 0x1D;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_track(w, &self.track_namespace, &self.track_name)?;
		w.varint(self.track_alias)?;

		// GROUP_ORDER is illegal on PUBLISH on draft-18 (a peer closes the session with
		// PROTOCOL_VIOLATION), and from draft-22 one of the subscription's parameters, which
		// moved here. The publisher's preference is a DEFAULT_PUBLISHER_GROUP_ORDER track
		// property instead.
		let draft20 = Filter::is_draft20(version);
		encode_params!(w, version,
			0x09 => self.largest_location,
			0x10 => self.forward,
			0x20 => self.subscriber_priority.filter(|_| draft20),
			0x21 => self.filter.filter(|_| draft20),
			0x22 => self.group_order.filter(|_| draft20),
		);

		// Track Properties are the final field, so nothing may follow.
		self.properties.encode(w, version)?;

		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let (track_namespace, track_name) = decode_track(r)?;
		let track_name = Cow::Borrowed(track_name);
		let track_alias = r.varint()?;

		// GROUP_ORDER is legal on PUBLISH from draft-22, when the subscription parameters
		// moved here; before that it closes the session.
		//
		// The draft-22 parameters have to parse even though reverse publishing is
		// unsupported: an unlisted parameter fails the whole message, which would kill the
		// session instead of letting the request reach NOT_SUPPORTED. The timeouts are
		// dropped.
		decode_params!(r, version,
			0x02 => _object_delivery_timeout: Option<u64> where Filter::is_draft20(version),
			0x03 => authorization_token: Vec<super::token::RequestToken>,
			0x06 => _subgroup_delivery_timeout: Option<u64> where Filter::is_draft20(version),
			0x08 => _expires: Option<u64>,
			0x09 => largest_location: Option<Location>,
			0x10 => forward: Option<bool>,
			0x20 => subscriber_priority: Option<u8> where Filter::is_draft20(version),
			0x21 => filter: Option<Filter> where Filter::is_draft20(version),
			0x22 => group_order: Option<GroupOrder> where Filter::is_draft20(version),
		);
		let properties = Properties::decode(r, version)?;
		// Last, so a malformed field closes the session before a token refusal.
		super::token::single(authorization_token)?;

		Ok(Self {
			request_id,
			track_namespace,
			track_name,
			track_alias,
			largest_location,
			forward: forward.unwrap_or(true),
			properties,
			subscriber_priority,
			group_order,
			filter,
		})
	}
}

#[derive(Debug)]
pub struct PublishOk {
	pub forward: bool,
	/// SUBSCRIBER_PRIORITY, `None` when absent: the subscription keeps the one it had.
	pub subscriber_priority: Option<u8>,
	/// GROUP_ORDER (through draft-18), `None` when absent.
	pub group_order: Option<GroupOrder>,
	/// The Location Filter (through draft-18), `None` when absent.
	pub filter: Option<Filter>,
	/// NEW_GROUP_REQUEST (through draft-18), which a dynamic track judges as a join.
	pub new_group: Option<u64>,
}

impl<'a> Message<'a> for PublishOk {
	const ID: u64 = 0x1E;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			// Draft-22 moved the subscription parameters out of PUBLISH_OK; they belong to
			// PUBLISH and REQUEST_UPDATE now, so a PUBLISH_OK carries none of them.
			_ if Filter::is_draft20(version) => encode_params!(w, version,),
			_ => {
				// GROUP_ORDER left PUBLISH_OK in draft-22. Sending it there is a parameter
				// on the wrong message, which closes the session.
				let group_order = matches!(version, Version::Draft18)
					.then_some(self.group_order)
					.flatten();
				let new_group = matches!(version, Version::Draft18).then_some(self.new_group).flatten();
				encode_params!(w, version,
					0x10 => self.forward,
					0x20 => self.subscriber_priority,
					0x21 => self.filter,
					0x22 => group_order,
					0x32 => new_group,
				);
			}
		}

		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		// EXPIRES is ignored, as in SUBSCRIBE_OK. Delivery timeouts are legal on the drafts
		// that list them and dropped: nothing here acts on a publisher's timeout. Draft-22
		// moved the subscription parameters off PUBLISH_OK, GROUP_ORDER
		// included.
		decode_params!(r, version,
			0x02 => _delivery_timeout: Option<u64> where !Filter::is_draft20(version),
			0x06 => _subgroup_delivery_timeout: Option<u64> where matches!(version, Version::Draft18),
			0x08 => _expires: Option<u64>,
			0x10 => forward: Option<bool> where !Filter::is_draft20(version),
			0x20 => subscriber_priority: Option<u8> where !Filter::is_draft20(version),
			0x21 => filter: Option<Filter> where !Filter::is_draft20(version),
			0x22 => group_order: Option<GroupOrder> where matches!(version, Version::Draft18),
			0x32 => new_group: Option<u64> where matches!(version, Version::Draft18),
		);

		Ok(Self {
			forward: forward.unwrap_or(true),
			subscriber_priority,
			group_order,
			filter,
			new_group,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Draft-22 moved the subscription parameters into PUBLISH. We refuse the request
	/// itself, but the message still has to parse: an unlisted parameter fails the whole
	/// decode, which kills the session instead of letting the peer get its refusal.
	#[test]
	fn publish_accepts_the_relocated_subscription_parameters() -> Result<(), EncodeError> {
		let version = Version::Draft22;
		let mut body = Vec::new();
		let w = &mut Encoder::new(&mut body, version.into());
		RequestId(1).encode(w, version)?;
		super::super::namespace::encode_track(w, &crate::Path::from_static("broadcast"), "video")?;
		w.varint(1u64)?; // track alias

		// SUBSCRIBER_PRIORITY then LOCATION_FILTER, delta encoded from 0.
		encode_params!(w, version,
			0x20 => 128u8,
			0x21 => Filter::NextObject,
		);
		Properties::default().encode(w, version)?;

		Publish::decode_msg(&mut Decoder::new(&body, version.into()), version)
			.expect("draft-22 PUBLISH parameters must parse");
		Ok(())
	}

	/// They arrived in draft-22, so an earlier peer sending one is still a violation.
	#[test]
	fn older_drafts_reject_the_relocated_parameters() -> Result<(), EncodeError> {
		let version = Version::Draft18;
		let mut body = Vec::new();
		let w = &mut Encoder::new(&mut body, version.into());
		RequestId(1).encode(w, version)?;
		super::super::namespace::encode_track(w, &crate::Path::from_static("broadcast"), "video")?;
		w.varint(1u64)?;
		encode_params!(w, version, 0x20 => 128u8);
		Properties::default().encode(w, version)?;

		assert!(Publish::decode_msg(&mut Decoder::new(&body, version.into()), version).is_err());
		Ok(())
	}

	fn encode_message<'m, M: Message<'m>>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf.to_vec()
	}

	fn decode_message<'m, M: Message<'m>>(bytes: &'m [u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes;
		crate::coding::decode_from(&mut buf, version, |r, v| M::decode_msg(r, v))
	}

	fn publish_with_param(version: Version, key: u64, value: u64) -> Vec<u8> {
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, version.into());
		RequestId(1).encode(w, version).unwrap();
		encode_track(w, &Path::from_static("t"), "v").unwrap();
		w.varint(1).unwrap();
		w.varint(1).unwrap();
		w.varint(key).unwrap();
		w.varint(value).unwrap();
		buf
	}

	fn publish_ok_with_param(version: Version, key: u64, value: u64) -> Vec<u8> {
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, version.into());
		w.varint(1).unwrap();
		w.varint(key).unwrap();
		w.varint(value).unwrap();
		buf
	}

	/// SUBSCRIBER_PRIORITY is not defined for PUBLISH until draft-22. OBJECT_DELIVERY_TIMEOUT
	/// is legal on PUBLISH again from draft-22.
	#[test]
	fn publish_parameters_follow_the_draft() {
		assert!(decode_message::<Publish>(&publish_with_param(Version::Draft18, 0x20, 1), Version::Draft18).is_err());
		decode_message::<Publish>(&publish_with_param(Version::Draft22, 0x02, 1), Version::Draft22)
			.expect("draft-22 accepts OBJECT_DELIVERY_TIMEOUT");
	}

	/// PUBLISH_OK carries the delivery timeout and NEW_GROUP_REQUEST through draft-18.
	/// Draft-22 no longer carries FORWARD.
	#[test]
	fn publish_ok_parameters_follow_the_draft() {
		decode_message::<PublishOk>(&publish_ok_with_param(Version::Draft18, 0x02, 1), Version::Draft18)
			.expect("draft-18 accepts DELIVERY_TIMEOUT");
		decode_message::<PublishOk>(&publish_ok_with_param(Version::Draft18, 0x32, 1), Version::Draft18)
			.expect("draft-18 accepts NEW_GROUP_REQUEST");
		assert!(
			decode_message::<PublishOk>(&publish_ok_with_param(Version::Draft22, 0x10, 0), Version::Draft22).is_err()
		);
	}

	#[test]
	fn test_publish_done_registered_statuses_all_versions() {
		for version in [Version::Draft18] {
			for (status, expected) in [
				(PublishDoneStatus::InternalError, 0x0),
				(PublishDoneStatus::TrackEnded, 0x2),
			] {
				assert_eq!(status.code(), expected);

				let msg = PublishDone {
					status_code: status.code(),
					stream_count: 5,
					reason_phrase: "done".into(),
				};
				let encoded = encode_message(&msg, version);
				let decoded: PublishDone = decode_message(&encoded, version).unwrap();

				assert_eq!(decoded.status_code, expected);
				assert_eq!(decoded.stream_count, 5);
				assert_eq!(decoded.reason_phrase, "done");
			}
		}
	}

	/// A subscriber ends the track the way the publisher said it ended: cleanly for a
	/// finished track or a reached filter end, with the publisher's code otherwise.
	#[test]
	fn publish_done_end_follows_the_status() {
		let done = |status_code| PublishDone {
			status_code,
			stream_count: 0,
			reason_phrase: "".into(),
		};

		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			assert!(done(0x2).end(version).is_ok(), "{version:?}");
			assert!(
				matches!(done(0x1).end(version), Err(crate::Error::Unauthorized)),
				"{version:?}"
			);
			assert!(
				matches!(done(0x0).end(version), Err(crate::Error::Remote(0x0))),
				"{version:?}"
			);
		}

		// SUBSCRIPTION_ENDED is clean until draft-22 unassigned it.
		assert!(done(0x3).end(Version::Draft18).is_ok());
		assert!(matches!(
			done(0x3).end(Version::Draft22),
			Err(crate::Error::Remote(0x3))
		));
	}

	#[test]
	fn test_publish_v18_round_trip() {
		let msg = Publish {
			request_id: RequestId(1),
			track_namespace: Path::from_static("test/ns"),
			track_name: "video".into(),
			track_alias: 42,
			largest_location: Some(Location { group: 10, object: 5 }),
			forward: true,
			properties: Properties {
				group_order: Some(GroupOrder::Descending),
				..Default::default()
			},
			subscriber_priority: None,
			group_order: None,
			filter: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: Publish = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test/ns");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.track_alias, 42);
		assert_eq!(decoded.largest_location, Some(Location { group: 10, object: 5 }));
		assert!(decoded.forward);
	}

	#[test]
	fn publish_priority_property_bytes_on_every_draft() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut msg = Publish {
				request_id: RequestId(1),
				track_namespace: Path::from_static("ns"),
				track_name: "video".into(),
				track_alias: 42,
				largest_location: None,
				forward: true,
				properties: Properties::default(),
				subscriber_priority: None,
				group_order: None,
				filter: None,
			};
			let baseline = encode_message(&msg, version);
			msg.properties.priority = Some(37);
			let encoded = encode_message(&msg, version);
			assert_eq!(encoded, [baseline, vec![0x0e, 37]].concat(), "{version}");
			assert_eq!(
				decode_message::<Publish>(&encoded, version)
					.unwrap()
					.properties
					.priority,
				Some(37)
			);
		}
	}

	/// GROUP_ORDER (0x22) is not a legal PUBLISH *message parameter*; a peer closes the
	/// session with PROTOCOL_VIOLATION when it sees one. The publisher's preference
	/// belongs in the DEFAULT_PUBLISHER_GROUP_ORDER track property,
	/// which shares the number 0x22 in the separate property registry.
	#[test]
	fn test_publish_v18_group_order_is_a_property() {
		let msg = Publish {
			request_id: RequestId(1),
			track_namespace: Path::from_static("ns"),
			track_name: "video".into(),
			track_alias: 42,
			largest_location: None,
			forward: true,
			properties: Properties {
				unknown: Default::default(),
				dynamic_groups: false,
				compress: false,
				max_cache_duration: None,
				timescale: None,
				priority: None,
				group_order: Some(GroupOrder::Descending),
			},
			subscriber_priority: None,
			group_order: None,
			filter: None,
		};

		#[rustfmt::skip]
		let expected = vec![
			1,                            // request id
			1,                            // namespace tuple count
			2, b'n', b's',                // "ns"
			5, b'v', b'i', b'd', b'e', b'o', // "video"
			42,                           // track alias
			1,                            // one message parameter
			0x10, 0x01,                   // FORWARD = true
			0x22, 0x02,                   // DEFAULT_PUBLISHER_GROUP_ORDER = descending
		];
		assert_eq!(encode_message(&msg, Version::Draft18), expected);

		let decoded: Publish = decode_message(&expected, Version::Draft18).unwrap();
		assert_eq!(decoded.properties.group_order, Some(GroupOrder::Descending));
	}

	#[test]
	fn test_publish_ok_v18_round_trip() {
		let msg = PublishOk {
			forward: true,
			subscriber_priority: Some(128),
			group_order: Some(GroupOrder::Descending),
			filter: Some(Filter::NextObject),
			new_group: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: PublishOk = decode_message(&encoded, Version::Draft18).unwrap();

		assert!(decoded.forward);
		assert_eq!(decoded.subscriber_priority, Some(128));
	}

	/// Draft-18 lets PUBLISH_OK carry EXPIRES; it is ignored rather than rejected.
	#[test]
	fn test_publish_ok_ignores_expires() {
		let bytes = [0x01, 0x08, 0x05];

		let decoded: PublishOk = decode_message(&bytes, Version::Draft18).unwrap();
		assert!(decoded.forward);
	}

	/// An absent parameter stays absent, so the subscription keeps the one it had: draft-22
	/// has no GROUP_ORDER on PUBLISH_OK at all.
	#[test]
	fn test_publish_ok_absent_parameters_stay_absent() {
		// No parameters.
		let decoded: PublishOk = decode_message(&[0x00], Version::Draft18).unwrap();
		assert!(decoded.forward);
		assert_eq!(decoded.subscriber_priority, None);
		assert_eq!(decoded.group_order, None);
		assert_eq!(decoded.filter, None);
		assert_eq!(decoded.new_group, None);
	}

	/// NEW_GROUP_REQUEST is kept on the drafts that allow it on PUBLISH_OK.
	#[test]
	fn test_publish_ok_carries_new_group_request() {
		let version = Version::Draft18;
		let msg = PublishOk {
			forward: true,
			subscriber_priority: None,
			group_order: None,
			filter: None,
			new_group: Some(9),
		};
		let wire = encode_message(&msg, version);

		let decoded: PublishOk = decode_message(&wire, version).unwrap();
		assert_eq!(decoded.new_group, Some(9), "{version}");
	}

	#[test]
	fn test_publish_done_preserves_unknown_status() {
		const UNKNOWN: u64 = 0xface;
		let msg = PublishDone {
			status_code: UNKNOWN,
			stream_count: 5,
			reason_phrase: "extension".into(),
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: PublishDone = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.status_code, UNKNOWN);
	}
}
