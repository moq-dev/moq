//! IETF moq-transport subscribe messages (v14 + v15)

use std::borrow::Cow;

use crate::{
	Path,
	coding::*,
	ietf::{Fill, Filter, GroupOrder, Location, Param, Parameters, Properties, RequestId},
};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

/// The INCLUDE_PROPERTIES parameter (0x35), draft-20's opt-out from Track Properties.
///
/// Length prefixed despite holding a single byte. The Key-Value-Pair rule keys the framing
/// off the parameter id's parity and 0x35 is odd, so a Length is present even though the
/// parameter's own section calls the value a uint8. Parity is what the generic parser uses
/// to skip a parameter it does not know, so following the prose instead would desync every
/// parameter after this one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncludeProperties(pub bool);

impl Param for IncludeProperties {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		vec![u8::from(self.0)].encode(w, version)
	}

	fn param_decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match Vec::<u8>::decode(r, version)?[..] {
			// The draft allows exactly 0 or 1; anything else is a protocol violation.
			[0] => Ok(Self(false)),
			[1] => Ok(Self(true)),
			_ => Err(DecodeError::InvalidValue),
		}
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
	/// Which Objects the subscription delivers.
	pub filter: Filter,
	/// The draft-20 backfill request, if the subscriber asked for one.
	pub fill: Option<Fill>,
	/// Whether the subscriber wants Track Properties on the response (draft-20).
	pub properties_wanted: bool,
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

				let filter = Filter::decode(r, version)?;

				let _params = Parameters::decode(r, version)?;

				Ok(Self {
					request_id,
					track_namespace,
					track_name,
					subscriber_priority,
					group_order,
					filter,
					fill: None,
					properties_wanted: true,
				})
			}
			_ => {
				decode_params!(r, version,
					0x02 => _object_delivery_timeout: Option<u64>,
					0x04 => rendezvous_timeout: Option<u64>,
					0x06 => _subgroup_delivery_timeout: Option<u64>,
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => filter: Option<Filter>,
					0x22 => group_order: Option<GroupOrder>,
					0x23 => fill: Option<Fill>,
					0x35 => include_properties: Option<IncludeProperties>,
				);

				// FILL_PARAMETERS and INCLUDE_PROPERTIES are draft-20 additions. An unknown
				// message parameter is a protocol violation, so they stay rejected on the
				// drafts that predate them rather than being quietly tolerated.
				if (fill.is_some() || include_properties.is_some()) && !Filter::is_draft20(version) {
					return Err(DecodeError::InvalidValue);
				}

				// Defaults to 1, so an absent parameter means the subscriber wants them.
				let properties_wanted = include_properties.is_none_or(|p| p.0);

				// RENDEZVOUS_TIMEOUT arrived in draft-17; 0x04 means MAX_CACHE_DURATION in
				// draft-15, which is a publisher parameter with no business in a SUBSCRIBE.
				// An unknown message parameter is a protocol violation, so reject it there.
				if rendezvous_timeout.is_some() && matches!(version, Version::Draft15 | Version::Draft16) {
					return Err(DecodeError::InvalidValue);
				}

				// The value is deliberately dropped: we always answer a SUBSCRIBE with what is
				// published right now, which is the shorter timeout the draft lets a relay pick.
				// We still have to parse it, or the parameter alone would kill the session.
				let _ = rendezvous_timeout;

				if forward == Some(false) {
					return Err(DecodeError::Unsupported);
				}

				let subscriber_priority = subscriber_priority.unwrap_or(128);
				let group_order = group_order.unwrap_or(GroupOrder::Descending);
				// An absent LOCATION_FILTER means the subscription is unfiltered.
				let filter = filter.unwrap_or(Filter::Unfiltered);

				Ok(Self {
					request_id,
					track_namespace,
					track_name,
					subscriber_priority,
					group_order,
					filter,
					fill,
					properties_wanted,
				})
			}
		}
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			0u64.encode(w, version)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}
		encode_namespace(w, &self.track_namespace, version)?;
		self.track_name.encode(w, version)?;

		match version {
			Version::Draft14 => {
				self.subscriber_priority.encode(w, version)?;
				self.group_order.encode(w, version)?;
				true.encode(w, version)?; // forward

				self.filter.encode(w, version)?;
				0u8.encode(w, version)?; // no parameters
			}
			_ => {
				// FILL_PARAMETERS arrived in draft-20. Sending it to an older peer would be an
				// unknown parameter, which is a protocol violation, so it is dropped instead.
				// A subscriber there simply joins mid-group, which is what it did all along.
				let fill = self.fill.filter(|_| Filter::is_draft20(version));

				// INCLUDE_PROPERTIES defaults to 1, so only the opt-out is worth bytes. It
				// arrived in draft-20, and an older peer would read it as an unknown
				// parameter, which is a protocol violation.
				let include_properties =
					(!self.properties_wanted && Filter::is_draft20(version)).then_some(IncludeProperties(false));

				encode_params!(w, version,
					0x10 => true,
					0x20 => self.subscriber_priority,
					0x21 => self.filter,
					0x22 => self.group_order,
					0x23 => fill,
					0x35 => include_properties,
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

	/// The largest Location in the track (LARGEST_OBJECT, 0x09), which the spec requires
	/// once the track has content. It is what a subscriber sizes a fill against.
	///
	/// Encoded on draft-20 only: the parameter is legal on earlier drafts too, but peers
	/// built before we sent it reject an unexpected SUBSCRIBE_OK parameter by closing the
	/// session, so emitting it there would break existing deployments over a hint.
	pub largest: Option<Location>,

	/// Metadata about the track, sent as Track Properties (draft-17+).
	pub properties: Properties,
}

impl Message for SubscribeOk {
	const ID: u64 = 0x04;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			self.request_id
				.expect("request_id required for draft14-16")
				.encode(w, version)?;
		} else {
			assert!(self.request_id.is_none(), "request_id must be None for draft17+");
		}
		self.track_alias.encode(w, version)?;

		match version {
			Version::Draft14 => {
				0u64.encode(w, version)?; // expires = 0
				self.properties
					.group_order
					.unwrap_or(GroupOrder::Ascending)
					.encode(w, version)?;
				false.encode(w, version)?; // no content
				0u8.encode(w, version)?; // no parameters
			}
			_ => {
				// GROUP_ORDER is a legal SUBSCRIBE_OK parameter only through draft-15; a later
				// peer closes the session with PROTOCOL_VIOLATION when it sees one. The
				// publisher's preference is a DEFAULT_PUBLISHER_GROUP_ORDER track property
				// instead, which we write from draft-17 on. Draft-16 gets neither form; see
				// Properties::encode.
				let group_order = match version {
					Version::Draft15 => self.properties.group_order,
					_ => None,
				};

				// See the field doc for why LARGEST_OBJECT stays draft-20 only.
				let largest = self.largest.filter(|_| Filter::is_draft20(version));

				encode_params!(w, version,
					0x09 => largest,
					0x22 => group_order,
				);

				// Track Properties are the final field, so nothing may follow.
				self.properties.encode(w, version)?;
			}
		}

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			Some(RequestId::decode(r, version)?)
		} else {
			None
		};
		let track_alias = u64::decode(r, version)?;
		let mut properties = Properties::default();

		match version {
			Version::Draft14 => {
				let expires = u64::decode(r, version)?;
				if expires != 0 {
					return Err(DecodeError::Unsupported);
				}

				properties.group_order = Some(GroupOrder::decode(r, version)?.any_to_descending());

				if bool::decode(r, version)? {
					let _group = u64::decode(r, version)?;
					let _object = u64::decode(r, version)?;
				}

				let _params = Parameters::decode(r, version)?;
			}
			_ => {
				// GROUP_ORDER is only legal here through draft-15, but keep accepting it so a
				// peer that still sends it doesn't have its session torn down over a hint.
				// LARGEST_OBJECT is required on every draft once the track has content, so
				// rejecting it would tear down a session over a parameter compliant
				// publishers must send.
				decode_params!(r, version,
					0x09 => largest: Option<Location>,
					0x22 => group_order: Option<GroupOrder>,
				);
				properties = Properties::decode(r, version)?;
				properties.group_order = properties.group_order.or(group_order);

				return Ok(Self {
					request_id,
					track_alias,
					largest,
					properties,
				});
			}
		}

		Ok(Self {
			request_id,
			track_alias,
			largest: None,
			properties,
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
#[derive(Clone, Debug)]
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
					0x21 => Filter::NextObject,
				);
			}
			_ => {
				assert!(
					self.subscription_request_id.is_none(),
					"subscription_request_id must be None for draft17+"
				);
				// REQUEST_UPDATE
				self.request_id.encode(w, version)?;
				if matches!(version, Version::Draft17) {
					0u64.encode(w, version)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
				}
				encode_params!(w, version,
					0x10 => self.forward,
					0x20 => self.subscriber_priority,
					0x21 => Filter::NextObject,
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
					0x02 => _object_delivery_timeout: Option<u64>,
					0x06 => _subgroup_delivery_timeout: Option<u64>,
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => _filter: Option<Filter>,
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
			_ => {
				// REQUEST_UPDATE
				let request_id = RequestId::decode(r, version)?;
				if matches!(version, Version::Draft17) {
					let _required_request_id_delta = u64::decode(r, version)?;
				}
				decode_params!(r, version,
					0x02 => _object_delivery_timeout: Option<u64>,
					0x06 => _subgroup_delivery_timeout: Option<u64>,
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => _filter: Option<Filter>,
					0x23 => fill: Option<Fill>,
				);

				// FILL_PARAMETERS is a draft-20 addition, so an earlier peer sending one is
				// still the protocol violation it was.
				if fill.is_some() && !Filter::is_draft20(version) {
					return Err(DecodeError::InvalidValue);
				}

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
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
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
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.subscriber_priority, 128);
	}

	/// Build a SUBSCRIBE body carrying a single RENDEZVOUS_TIMEOUT parameter.
	fn subscribe_with_rendezvous(millis: u64, version: Version) -> Vec<u8> {
		fn build(millis: u64, version: Version) -> Result<Vec<u8>, EncodeError> {
			let mut buf = BytesMut::new();
			RequestId(1).encode(&mut buf, version)?;
			if version == Version::Draft17 {
				0u64.encode(&mut buf, version)?; // required_request_id_delta
			}
			encode_namespace(&mut buf, &Path::new("test"), version)?;
			Cow::Borrowed("video").encode(&mut buf, version)?;
			encode_params!(&mut buf, version,
				0x04 => millis,
			);
			Ok(buf.to_vec())
		}

		build(millis, version).unwrap()
	}

	/// A subscriber may send RENDEZVOUS_TIMEOUT (draft-17+). We do not honor the wait, but an
	/// unknown message parameter is a session-killing protocol violation, so it still has to
	/// parse: failing to decode it takes the whole session down instead of answering the
	/// SUBSCRIBE.
	#[test]
	fn rendezvous_timeout_is_accepted_and_ignored() {
		for version in [Version::Draft17, Version::Draft18, Version::Draft19, Version::Draft20] {
			for millis in [0, 5000] {
				let encoded = subscribe_with_rendezvous(millis, version);
				let msg: Subscribe = decode_message(&encoded, version)
					.unwrap_or_else(|err| panic!("{version} rendezvous {millis}ms: {err}"));
				assert_eq!(msg.track_name, "video");
			}
		}
	}

	/// 0x04 only means RENDEZVOUS_TIMEOUT from draft-17 on; in draft-15 it is
	/// MAX_CACHE_DURATION, a publisher parameter that has no business in a SUBSCRIBE.
	#[test]
	fn rendezvous_timeout_is_rejected_before_draft17() {
		for version in [Version::Draft15, Version::Draft16] {
			let encoded = subscribe_with_rendezvous(0, version);
			decode_message::<Subscribe>(&encoded, version).expect_err(&format!("{version} must reject parameter 0x04"));
		}
	}

	/// The first message parameter key on an encoded SUBSCRIBE, or `None` when it carries no
	/// parameters.
	///
	/// Only the first key is needed, and only the first is readable. `encode_params!` enforces
	/// ascending keys at compile time and RENDEZVOUS_TIMEOUT (0x04) sorts below every parameter
	/// we do send, so it can only appear here. Walking further is not possible anyway: message
	/// parameter values are typed per key with no generic skip rule, which is exactly why the
	/// draft makes an unknown one a protocol violation.
	fn first_param_key(encoded: &[u8], version: Version) -> Option<u64> {
		let mut buf = bytes::Bytes::copy_from_slice(encoded);
		RequestId::decode(&mut buf, version).unwrap();
		if version == Version::Draft17 {
			u64::decode(&mut buf, version).unwrap();
		}
		decode_namespace(&mut buf, version).unwrap();
		Cow::<str>::decode(&mut buf, version).unwrap();

		// draft-14/15 write absolute keys, draft-16+ deltas, but the first is absolute either way.
		let count = u64::decode(&mut buf, version).unwrap();
		(count > 0).then(|| u64::decode(&mut buf, version).unwrap())
	}

	/// We never ask a peer to hold a subscription open, so the parameter stays off our wire.
	#[test]
	fn rendezvous_timeout_is_never_sent() {
		let msg = Subscribe {
			request_id: RequestId(1),
			track_namespace: Path::new("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
		};

		for version in [Version::Draft17, Version::Draft18, Version::Draft19, Version::Draft20] {
			// The reader has to be able to see a 0x04, or its absence below proves nothing.
			assert_eq!(
				first_param_key(&subscribe_with_rendezvous(5000, version), version),
				Some(0x04),
				"{version}: reader missed a planted RENDEZVOUS_TIMEOUT"
			);

			assert_eq!(
				first_param_key(&encode_message(&msg, version), version),
				Some(0x10),
				"{version}: RENDEZVOUS_TIMEOUT must not be advertised"
			);
		}
	}

	#[test]
	fn test_subscribe_nested_namespace() {
		let msg = Subscribe {
			request_id: RequestId(100),
			track_namespace: Path::new("conference/room123"),
			track_name: "audio".into(),
			subscriber_priority: 255,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
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
			largest: None,
			properties: Properties::default(),
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(42)));
	}

	#[test]
	fn test_subscribe_ok_accepts_largest_object() {
		// request_id=4, track_alias=4, one LARGEST_OBJECT parameter at {5, 0}.
		let payload = [0x04, 0x04, 0x01, 0x09, 0x02, 0x05, 0x00];
		for version in [Version::Draft15, Version::Draft16] {
			let decoded: SubscribeOk = decode_message(&payload, version).unwrap();

			assert_eq!(decoded.request_id, Some(RequestId(4)));
			assert_eq!(decoded.track_alias, 4);
			assert_eq!(decoded.largest, Some(Location { group: 5, object: 0 }), "{version}");
		}
	}

	/// LARGEST_OBJECT is required in SUBSCRIBE_OK once the track has content, so a
	/// draft-17+ decoder must accept it rather than tearing down the session over a
	/// parameter compliant publishers have to send. The empty Track Properties block
	/// follows the parameters.
	#[test]
	fn test_subscribe_ok_accepts_largest_object_draft17_on() {
		let payload = [0x04, 0x01, 0x09, 0x02, 0x05, 0x00];
		for version in [Version::Draft17, Version::Draft18, Version::Draft19, Version::Draft20] {
			let decoded: SubscribeOk = decode_message(&payload, version).unwrap();

			assert_eq!(decoded.request_id, None, "{version}");
			assert_eq!(decoded.track_alias, 4, "{version}");
			assert_eq!(decoded.largest, Some(Location { group: 5, object: 0 }), "{version}");
		}
	}

	/// The encoder emits LARGEST_OBJECT on draft-20 only: it is legal earlier, but peers
	/// built before we sent it reject an unexpected SUBSCRIBE_OK parameter by closing the
	/// session.
	#[test]
	fn test_subscribe_ok_largest_object_round_trips_on_draft20_only() {
		let msg = SubscribeOk {
			request_id: None,
			track_alias: 7,
			largest: Some(Location { group: 9, object: 3 }),
			properties: Properties::default(),
		};

		let encoded = encode_message(&msg, Version::Draft20);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft20).unwrap();
		assert_eq!(decoded.largest, Some(Location { group: 9, object: 3 }));

		let encoded = encode_message(&msg, Version::Draft19);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft19).unwrap();
		assert_eq!(decoded.largest, None, "draft-19 must not carry the parameter");
	}

	#[test]
	fn test_subscribe_ok_v15() {
		let msg = SubscribeOk {
			request_id: Some(RequestId(42)),
			track_alias: 42,
			largest: None,
			properties: Properties::default(),
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

	/// Every parameter the draft lets a message carry has to parse, even the ones we act on
	/// nowhere: an unlisted key fails the whole message, which tears the session down
	/// instead of letting the request reach its answer.
	#[test]
	fn subscribe_accepts_the_delivery_timeouts() -> Result<(), EncodeError> {
		for version in [Version::Draft19, Version::Draft20] {
			let mut body = Vec::new();
			RequestId(1).encode(&mut body, version)?;
			encode_namespace(&mut body, &crate::Path::new("broadcast"), version)?;
			"video".encode(&mut body, version)?;
			encode_params!(&mut body, version,
				0x02 => 5000u64,
				0x06 => 9000u64,
			);

			let mut buf = bytes::Bytes::from(body);
			Subscribe::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
		}
		Ok(())
	}

	/// The opt-out has to reach the wire, or a subscriber that asked for no Track
	/// Properties decodes back as wanting them.
	#[test]
	fn include_properties_round_trips() {
		for (wanted, version) in [
			(false, Version::Draft20),
			(true, Version::Draft20),
			// Older drafts have no such parameter, so the opt-out is dropped rather than
			// sent as one they would treat as a protocol violation.
			(true, Version::Draft19),
		] {
			let msg = Subscribe {
				request_id: RequestId(1),
				track_namespace: crate::Path::new("broadcast"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: GroupOrder::Descending,
				filter: Filter::NextObject,
				fill: None,
				properties_wanted: wanted,
			};

			let encoded = encode_message(&msg, version);
			let decoded: Subscribe = decode_message(&encoded, version).unwrap();
			assert_eq!(decoded.properties_wanted, wanted, "{version}");
		}
	}

	#[test]
	fn test_subscribe_rejects_invalid_filter() {
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
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
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
			largest: None,
			properties: Properties::default(),
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

	#[test]
	fn test_subscribe_v18_round_trip() {
		let msg = Subscribe {
			request_id: RequestId(1),
			track_namespace: Path::new("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_subscribe_ok_v18_round_trip() {
		let msg = SubscribeOk {
			request_id: None,
			track_alias: 42,
			largest: None,
			properties: Properties::default(),
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, None);
		assert_eq!(decoded.track_alias, 42);
	}

	/// GROUP_ORDER (0x22) is only a legal SUBSCRIBE_OK *message parameter* through draft-15;
	/// a draft-16+ peer closes the session with PROTOCOL_VIOLATION when it sees one. The
	/// publisher's preference belongs in the DEFAULT_PUBLISHER_GROUP_ORDER track property,
	/// which shares the number 0x22 in the separate property registry.
	#[test]
	fn test_subscribe_ok_v18_group_order_is_a_property() {
		let msg = SubscribeOk {
			request_id: None,
			track_alias: 42,
			largest: None,
			properties: Properties {
				timescale: None,
				group_order: Some(GroupOrder::Descending),
			},
		};

		#[rustfmt::skip]
		let expected = vec![
			42,   // track alias
			0,    // zero message parameters
			0x22, // DEFAULT_PUBLISHER_GROUP_ORDER, the first (and only) track property
			0x02, // descending
		];
		assert_eq!(encode_message(&msg, Version::Draft18), expected);

		let decoded: SubscribeOk = decode_message(&expected, Version::Draft18).unwrap();
		assert_eq!(decoded.properties.group_order, Some(GroupOrder::Descending));
	}

	/// Draft-15 is the one version that takes it as a message parameter, and has no track
	/// properties to put it in.
	#[test]
	fn test_subscribe_ok_v15_group_order_is_a_parameter() {
		let msg = SubscribeOk {
			request_id: Some(RequestId(7)),
			track_alias: 42,
			largest: None,
			properties: Properties {
				timescale: None,
				group_order: Some(GroupOrder::Descending),
			},
		};

		#[rustfmt::skip]
		let expected = vec![
			7,    // request id
			42,   // track alias
			1,    // one message parameter
			0x22, // GROUP_ORDER
			0x02, // descending
		];
		assert_eq!(encode_message(&msg, Version::Draft15), expected);

		let decoded: SubscribeOk = decode_message(&expected, Version::Draft15).unwrap();
		assert_eq!(decoded.properties.group_order, Some(GroupOrder::Descending));
	}

	/// Draft-16 has the block too, under the name Track Extensions. We don't write one there,
	/// but a peer that does used to fail the whole message as `Long`.
	#[test]
	fn test_subscribe_ok_v16_reads_track_extensions() {
		#[rustfmt::skip]
		let body = vec![
			7,    // request id
			42,   // track alias
			0,    // zero message parameters
			0x22, // DEFAULT_PUBLISHER_GROUP_ORDER
			0x02, // descending
		];

		// Go through the size-prefixed path: that's what rejects unread trailing bytes.
		let mut buf = BytesMut::new();
		(body.len() as u16).encode(&mut buf, Version::Draft16).unwrap();
		buf.extend_from_slice(&body);

		let mut bytes = buf.freeze();
		let decoded = SubscribeOk::decode(&mut bytes, Version::Draft16).unwrap();
		assert_eq!(decoded.track_alias, 42);
		assert_eq!(decoded.properties.group_order, Some(GroupOrder::Descending));
	}

	/// We stopped sending the parameter, but a peer still sending it shouldn't lose its
	/// session over a hint we ignore anyway.
	#[test]
	fn test_subscribe_ok_v18_accepts_group_order_parameter() {
		#[rustfmt::skip]
		let bytes = vec![
			42,   // track alias
			1,    // one message parameter
			0x22, // GROUP_ORDER
			0x02, // descending
		];

		let decoded: SubscribeOk = decode_message(&bytes, Version::Draft18).unwrap();
		assert_eq!(decoded.track_alias, 42);
		assert_eq!(decoded.properties.group_order, Some(GroupOrder::Descending));
	}

	/// Draft-18 removes the `required_request_id_delta` field (#1615), so the
	/// REQUEST_UPDATE wire format is 1 varint shorter than draft-17.
	#[test]
	fn test_subscribe_update_v18_round_trip() {
		let msg = SubscribeUpdate {
			request_id: RequestId(10),
			subscription_request_id: None,
			start_location: Location { group: 0, object: 0 },
			end_group: 0,
			subscriber_priority: 200,
			forward: true,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: SubscribeUpdate = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(10));
		assert_eq!(decoded.subscription_request_id, None);
		assert_eq!(decoded.subscriber_priority, 200);
		assert!(decoded.forward);
	}

	/// Cross-check: draft-17 emits an extra 0-byte (required_request_id_delta) that
	/// draft-18 does not. So a draft-18 encoding should be exactly 1 byte shorter
	/// than draft-17 for SUBSCRIBE_UPDATE.
	#[test]
	fn test_subscribe_update_v17_v18_size_differs() {
		let v17_msg = SubscribeUpdate {
			request_id: RequestId(10),
			subscription_request_id: None,
			start_location: Location { group: 0, object: 0 },
			end_group: 0,
			subscriber_priority: 200,
			forward: true,
		};
		let v18_msg = SubscribeUpdate { ..v17_msg.clone() };

		let v17 = encode_message(&v17_msg, Version::Draft17);
		let v18 = encode_message(&v18_msg, Version::Draft18);
		assert_eq!(v17.len(), v18.len() + 1);
	}
}
