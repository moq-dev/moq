//! IETF moq-transport subscribe messages (v14 + v15)

use std::borrow::Cow;

use crate::{
	Path,
	coding::*,
	ietf::{Fill, Filter, GroupOrder, Location, Opaque, Parameters, Properties, RequestId},
};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

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
	/// Whether Objects are forwarded (FORWARD). We only serve forwarding subscriptions,
	/// so a subscriber that asks for none is refused.
	pub forward: bool,
	/// Whether the request carried a Range Filter (0x25-0x28). We advertise no
	/// MAX_FILTER_RANGES, so the request is refused rather than served unfiltered.
	/// Never encoded; we send no range filters.
	pub range_filters: bool,
}

/// NEW_GROUP_REQUEST (0x32) arrived in draft-16.
fn has_new_group_request(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15)
}

/// The Range Filters (0x25-0x29) arrived in draft-19.
pub(super) fn has_range_filters(version: Version) -> bool {
	!matches!(
		version,
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18
	)
}

impl Message for Subscribe<'_> {
	const ID: u64 = 0x03;

	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = r.varint()?;
		}
		let track_namespace = decode_namespace(r)?;
		let track_name = Cow::Owned(r.string()?);

		match version {
			Version::Draft14 => {
				let subscriber_priority = r.u8()?;
				let group_order = GroupOrder::decode(r, version)?;

				let forward = r.bool()?;
				let filter = Filter::decode(r, version)?;

				Parameters::skip(r)?;

				Ok(Self {
					request_id,
					track_namespace,
					track_name,
					subscriber_priority,
					group_order,
					filter,
					fill: None,
					properties_wanted: true,
					forward,
					range_filters: false,
				})
			}
			_ => {
				// The token is ignored: the session's grant is what authorizes the request.
				// NEW_GROUP_REQUEST is ignored too, as the draft lets a publisher without
				// dynamic groups do.
				decode_params!(r, version,
					0x02 => _object_delivery_timeout: Option<u64>,
					0x03 => _authorization_token: Vec<Opaque>,
					// 0x04 is MAX_CACHE_DURATION in draft-15 (a publisher parameter) and not a
					// message parameter at all in draft-16. RENDEZVOUS_TIMEOUT arrives in draft-17.
					0x04 => rendezvous_timeout: Option<u64> where !matches!(version, Version::Draft15 | Version::Draft16),
					0x06 => _subgroup_delivery_timeout: Option<u64> where !matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17),
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => filter: Option<Filter>,
					0x22 => group_order: Option<GroupOrder>,
					0x23 => fill: Option<Fill> where Filter::is_draft20(version),
					0x25 => subgroup_filter: Vec<Opaque> where has_range_filters(version),
					0x26 => object_id_filter: Vec<Opaque> where has_range_filters(version),
					0x27 => priority_filter: Vec<Opaque> where has_range_filters(version),
					0x28 => object_property_filter: Vec<Opaque> where has_range_filters(version),
					0x32 => _new_group_request: Option<u64> where has_new_group_request(version),
					0x35 => include_properties: Option<bool> where Filter::is_draft20(version),
				);

				let range_filters = [
					subgroup_filter,
					object_id_filter,
					priority_filter,
					object_property_filter,
				]
				.iter()
				.any(|filter| !filter.is_empty());

				// Defaults to 1, so an absent parameter means the subscriber wants them.
				let properties_wanted = include_properties.unwrap_or(true);

				// The value is deliberately dropped: we always answer a SUBSCRIBE with what is
				// published right now, which is the shorter timeout the draft lets a relay pick.
				// We still have to parse it, or the parameter alone would kill the session.
				let _ = rendezvous_timeout;

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
					// Absent means 1.
					forward: forward.unwrap_or(true),
					range_filters,
				})
			}
		}
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			w.varint(0)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}
		encode_namespace(w, &self.track_namespace)?;
		w.string(&self.track_name)?;

		match version {
			Version::Draft14 => {
				w.u8(self.subscriber_priority);
				self.group_order.encode(w, version)?;
				w.bool(self.forward);

				self.filter.encode(w, version)?;
				w.u8(0); // no parameters
			}
			_ => {
				// FILL_PARAMETERS arrived in draft-20. Sending it to an older peer would be an
				// unknown parameter, which is a protocol violation, so it is dropped instead.
				// A subscriber there simply joins mid-group, which is what it did all along.
				let fill = self.fill.filter(|_| Filter::is_draft20(version));

				// INCLUDE_PROPERTIES defaults to 1, so only the opt-out is worth bytes. It
				// arrived in draft-20, and an older peer would read it as an unknown
				// parameter, which is a protocol violation.
				let include_properties = (!self.properties_wanted && Filter::is_draft20(version)).then_some(false);

				encode_params!(w, version,
					0x10 => self.forward,
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
	pub largest: Option<Location>,

	/// Metadata about the track, sent as Track Properties (draft-17+).
	pub properties: Properties,
}

impl Message for SubscribeOk {
	const ID: u64 = 0x04;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			self.request_id
				.expect("request_id required for draft14-16")
				.encode(w, version)?;
		} else {
			assert!(self.request_id.is_none(), "request_id must be None for draft17+");
		}
		w.varint(self.track_alias)?;

		match version {
			Version::Draft14 => {
				w.varint(0)?; // expires = 0
				self.properties
					.group_order
					.unwrap_or(GroupOrder::Ascending)
					.encode(w, version)?;
				w.bool(self.largest.is_some());
				if let Some(largest) = self.largest {
					largest.encode(w, version)?;
				}
				w.u8(0); // no parameters
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

				encode_params!(w, version,
					0x09 => self.largest,
					0x22 => group_order,
				);

				// Track Properties are the final field, so nothing may follow.
				self.properties.encode(w, version)?;
			}
		}

		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let request_id = if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			Some(RequestId::decode(r, version)?)
		} else {
			None
		};
		let track_alias = r.varint()?;
		let mut properties = Properties::default();
		let mut largest = None;

		match version {
			Version::Draft14 => {
				// EXPIRES is when the publisher expects to end the subscription. That end
				// arrives as PUBLISH_DONE regardless, so there is nothing to act on.
				let _expires = r.varint()?;

				properties.group_order = Some(GroupOrder::decode(r, version)?.any_to_descending());

				if r.bool()? {
					largest = Some(Location::decode(r, version)?);
				}

				properties.max_cache_duration = Parameters::skip(r)?.map(std::time::Duration::from_millis);
			}
			_ => {
				// GROUP_ORDER and MAX_CACHE_DURATION are legal here only in draft-15. Draft-16
				// still knows GROUP_ORDER, so one on this message is ignored. From draft-17
				// it closes the session. LARGEST_OBJECT is required once the track has
				// content, so rejecting it would tear down a session over a parameter
				// compliant publishers must send. EXPIRES is ignored, as on draft-14.
				decode_params!(r, version,
					0x04 => max_cache_duration: Option<u64> where version == Version::Draft15,
					0x08 => _expires: Option<u64>,
					0x09 => largest: Option<Location>,
					0x22 => group_order: Option<GroupOrder> where version == Version::Draft15,
				);
				properties = Properties::decode(r, version)?;
				properties.group_order = properties.group_order.or(group_order);
				if version == Version::Draft15 {
					properties.max_cache_duration = max_cache_duration.map(std::time::Duration::from_millis);
				}

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
			largest,
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

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		w.varint(self.error_code)?;
		w.string(&self.reason_phrase)?;
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let error_code = r.varint()?;
		let reason_phrase = Cow::Owned(r.string()?);

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

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
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

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft14 => {
				self.request_id.encode(w, version)?;
				self.subscription_request_id
					.expect("subscription_request_id required for draft14")
					.encode(w, version)?;
				self.start_location.encode(w, version)?;
				w.varint(self.end_group)?;
				w.u8(self.subscriber_priority);
				w.bool(self.forward);
				w.u8(0); // no parameters
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
					w.varint(0)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
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

	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 => {
				let request_id = RequestId::decode(r, version)?;
				let subscription_request_id = Some(RequestId::decode(r, version)?);
				let start_location = Location::decode(r, version)?;
				let end_group = r.varint()?;
				let subscriber_priority = r.u8()?;
				let forward = r.bool()?;
				Parameters::skip(r)?;

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
					0x03 => _authorization_token: Vec<Opaque>,
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => _filter: Option<Filter>,
					0x32 => _new_group_request: Option<u64> where has_new_group_request(version),
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
					let _required_request_id_delta = r.varint()?;
				}
				// Nothing reads an update's FILL_PARAMETERS or Range Filters yet; they are
				// consumed so a legal update does not fail the session.
				decode_params!(r, version,
					0x02 => _object_delivery_timeout: Option<u64>,
					0x03 => _authorization_token: Vec<Opaque>,
					0x06 => _subgroup_delivery_timeout: Option<u64> where !matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17),
					0x10 => forward: Option<bool>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => _filter: Option<Filter>,
					0x23 => _fill: Option<Fill> where Filter::is_draft20(version),
					0x25 => _subgroup_filter: Vec<Opaque> where has_range_filters(version),
					0x26 => _object_id_filter: Vec<Opaque> where has_range_filters(version),
					0x27 => _priority_filter: Vec<Opaque> where has_range_filters(version),
					0x28 => _object_property_filter: Vec<Opaque> where has_range_filters(version),
					0x29 => _track_property_filter: Vec<Opaque> where has_range_filters(version),
					0x32 => _new_group_request: Option<u64>,
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
			forward: true,
			range_filters: false,
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
			forward: true,
			range_filters: false,
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
			let mut buf = Vec::new();
			let w = &mut Encoder::new(&mut buf, version.into());
			RequestId(1).encode(w, version)?;
			if version == Version::Draft17 {
				w.varint(0)?; // required_request_id_delta
			}
			encode_namespace(w, &Path::new("test"))?;
			w.string("video")?;
			encode_params!(w, version,
				0x04 => millis,
			);
			Ok(buf)
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

	/// 0x04 is MAX_CACHE_DURATION in draft-15, defined for other messages, so a SUBSCRIBE
	/// that carries it is ignored. Draft-16 dropped the id, which makes it unknown.
	#[test]
	fn cache_duration_on_subscribe_follows_the_draft() {
		let draft15 = subscribe_with_rendezvous(0, Version::Draft15);
		decode_message::<Subscribe>(&draft15, Version::Draft15).expect("draft-15 ignores MAX_CACHE_DURATION");

		let draft16 = subscribe_with_rendezvous(0, Version::Draft16);
		decode_message::<Subscribe>(&draft16, Version::Draft16).expect_err("draft-16 rejects unknown parameter 0x04");
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
		let r = &mut Decoder::new(encoded, version.into());
		RequestId::decode(r, version).unwrap();
		if version == Version::Draft17 {
			r.varint().unwrap();
		}
		decode_namespace(r).unwrap();
		r.string().unwrap();

		// draft-14/15 write absolute keys, draft-16+ deltas, but the first is absolute either way.
		let count = r.varint().unwrap();
		(count > 0).then(|| r.varint().unwrap())
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
			forward: true,
			range_filters: false,
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
			forward: true,
			range_filters: false,
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

	/// From draft-17 the parameter value is two bare varints, so the same Location costs
	/// one byte less than the length-prefixed form above. The empty Track Properties block
	/// follows the parameters.
	#[test]
	fn test_subscribe_ok_accepts_largest_object_draft17_on() {
		let payload = [0x04, 0x01, 0x09, 0x05, 0x00];
		for version in [Version::Draft17, Version::Draft18, Version::Draft19, Version::Draft20] {
			let decoded: SubscribeOk = decode_message(&payload, version).unwrap();

			assert_eq!(decoded.request_id, None, "{version}");
			assert_eq!(decoded.track_alias, 4, "{version}");
			assert_eq!(decoded.largest, Some(Location { group: 5, object: 0 }), "{version}");
		}
	}

	/// The regression from #3558: a draft-18 peer sends the Group as a two-byte varint, and
	/// reading the parameter as length-prefixed turned that first byte into a demand for 427
	/// bytes of value, failing the message with a short buffer.
	#[test]
	fn test_subscribe_ok_largest_object_multibyte_group_draft18() {
		#[rustfmt::skip]
		let payload = [
			0x04, // track alias
			0x01, // one message parameter
			0x09, // LARGEST_OBJECT
			0x81, 0xab, // group 427, a two-byte leading-ones varint
			0x00, // object 0
		];

		let decoded: SubscribeOk = decode_message(&payload, Version::Draft18).unwrap();
		assert_eq!(decoded.largest, Some(Location { group: 427, object: 0 }));
	}

	/// LARGEST_OBJECT is required once the track has content, so every draft that defines
	/// it carries it: a content flag on draft-14, a length-prefixed parameter through
	/// draft-16, and a bare Location parameter after.
	#[test]
	fn test_subscribe_ok_largest_object_wire_vectors() {
		let largest = Some(Location { group: 9, object: 3 });

		for (version, expected) in [
			(Version::Draft14, &[7, 42, 0, 1, 1, 9, 3, 0][..]),
			(Version::Draft16, &[7, 42, 1, 0x09, 0x02, 0x09, 0x03][..]),
			(Version::Draft17, &[42, 1, 0x09, 0x09, 0x03][..]),
			(Version::Draft18, &[42, 1, 0x09, 0x09, 0x03][..]),
			(Version::Draft19, &[42, 1, 0x09, 0x09, 0x03][..]),
			(Version::Draft20, &[42, 1, 0x09, 0x09, 0x03][..]),
		] {
			let msg = SubscribeOk {
				request_id: matches!(version, Version::Draft14 | Version::Draft16).then_some(RequestId(7)),
				track_alias: 42,
				largest,
				properties: Properties::default(),
			};

			assert_eq!(encode_message(&msg, version), expected, "{version}");

			let decoded: SubscribeOk = decode_message(expected, version).unwrap();
			assert_eq!(decoded.largest, largest, "{version}");
		}
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
			let w = &mut Encoder::new(&mut body, version.into());
			RequestId(1).encode(w, version)?;
			encode_namespace(w, &crate::Path::new("broadcast"))?;
			w.string("video")?;
			encode_params!(w, version,
				0x02 => 5000u64,
				0x06 => 9000u64,
			);

			Subscribe::decode_msg(&mut Decoder::new(&body, version.into()), version)
				.unwrap_or_else(|e| panic!("{version}: {e}"));
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
				forward: true,
				range_filters: false,
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
	fn test_subscribe_ok_ignores_expires_v14() {
		#[rustfmt::skip]
		let bytes = [
			0x01, // request_id
			0x00, // track_alias
			0x05, // expires = 5
			0x02, // group_order
			0x00, // content_exists
			0x00, // num_params
		];

		let decoded: SubscribeOk = decode_message(&bytes, Version::Draft14).unwrap();
		assert_eq!(decoded.request_id, Some(RequestId(1)));
	}

	/// The SUBSCRIBE_OK aiomoqt 0.11.0 sends: EXPIRES = 0 and nothing else (#4172).
	#[test]
	fn test_subscribe_ok_ignores_expires_aiomoqt() {
		let bytes = [0x03, 0x00, 0x01, 0x08, 0x00];

		let decoded: SubscribeOk = decode_message(&bytes, Version::Draft16).unwrap();
		assert_eq!(decoded.request_id, Some(RequestId(3)));
		assert_eq!(decoded.track_alias, 0);
		assert!(decoded.largest.is_none());
	}

	/// The SUBSCRIBE_OK a libquicr relay sends: EXPIRES = 0 followed by five track
	/// properties (#4172).
	#[test]
	fn test_subscribe_ok_ignores_expires_libquicr() {
		let bytes = [
			0x02, 0xde, 0x53, 0x15, 0xbf, 0xd6, 0x39, 0x31, 0x88, 0x01, 0x08, 0x00, 0x02, 0x00, 0x02, 0x00, 0x0a, 0x01,
			0x14, 0x01, 0x0e, 0x01,
		];

		let decoded: SubscribeOk = decode_message(&bytes, Version::Draft16).unwrap();
		assert_eq!(decoded.request_id, Some(RequestId(2)));
		assert_eq!(decoded.properties.priority, Some(1));
		assert_eq!(decoded.properties.group_order, Some(GroupOrder::Ascending));
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
			forward: true,
			range_filters: false,
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
			forward: true,
			range_filters: false,
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

	/// The registered priority property is two literal bytes after the message body on
	/// draft-17+, and absent on older drafts. The decoder returns the same wire value.
	#[test]
	fn subscribe_ok_priority_property_bytes_on_every_draft() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			let mut msg = SubscribeOk {
				request_id: matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16)
					.then_some(RequestId(7)),
				track_alias: 42,
				largest: None,
				properties: Properties::default(),
			};
			let baseline = encode_message(&msg, version);
			msg.properties.priority = Some(37);
			let encoded = encode_message(&msg, version);
			if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
				assert_eq!(encoded, baseline, "{version}");
			} else {
				assert_eq!(encoded, [baseline, vec![0x0e, 37]].concat(), "{version}");
				assert_eq!(
					decode_message::<SubscribeOk>(&encoded, version)
						.unwrap()
						.properties
						.priority,
					Some(37)
				);
			}
		}
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
				max_cache_duration: None,
				timescale: None,
				priority: None,
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
				max_cache_duration: None,
				timescale: None,
				priority: None,
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
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Draft16.into()).u16(body.len() as u16);
		buf.extend_from_slice(&body);

		let mut bytes = bytes::Bytes::from(buf);
		let decoded = crate::coding::decode_buf(&mut bytes, Version::Draft16, SubscribeOk::decode).unwrap();
		assert_eq!(decoded.track_alias, 42);
		assert_eq!(decoded.properties.group_order, Some(GroupOrder::Descending));
	}

	/// GROUP_ORDER left SUBSCRIBE_OK after draft-15. Draft-18 closes the session when a
	/// known parameter shows up on a message that does not define it.
	#[test]
	fn test_subscribe_ok_v18_rejects_group_order_parameter() {
		#[rustfmt::skip]
		let bytes = vec![
			42,   // track alias
			1,    // one message parameter
			0x22, // GROUP_ORDER
			0x02, // descending
		];

		assert!(decode_message::<SubscribeOk>(&bytes, Version::Draft18).is_err());
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

	/// The head of a SUBSCRIBE (draft-20 Figure 10) up to its Number of Parameters:
	/// Request ID 1, Track Namespace ("live"), Track Name ("video"). Every value is
	/// below 128, so each varint is one byte in the drafts before and after 17 alike.
	const SUBSCRIBE_HEAD: &[u8] = &[
		0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x05, b'v', b'i', b'd', b'e', b'o',
	];

	fn subscribe_body(params: &[u8]) -> Vec<u8> {
		[SUBSCRIBE_HEAD, params].concat()
	}

	/// Every parameter draft-20 lets a SUBSCRIBE carry that we don't act on still decodes,
	/// with what we have to refuse recorded rather than failing the session.
	#[test]
	fn subscribe_decodes_every_draft20_parameter() {
		#[rustfmt::skip]
		let body = subscribe_body(&[
			0x06, // Number of Parameters
			0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN: USE_VALUE, type 0, one byte
			0x00, 0x03, 0x03, 0x00, 0xBB, // a second one, which the draft allows
			0x0D, 0x00, // FORWARD (0x10) = 0, a uint8
			0x15, 0x03, 0x00, 0x00, 0x01, // SUBGROUP_FILTER (0x25): SetID 0, range 0-1
			0x0D, 0x07, // NEW_GROUP_REQUEST (0x32), a varint
			0x03, 0x00, // INCLUDE_PROPERTIES (0x35) = 0, a uint8
		]);

		for version in [Version::Draft20, Version::Draft21, Version::Draft22] {
			let wire = body.clone();
			let mut buf = Decoder::new(&wire, version.into());
			let msg = Subscribe::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
			assert!(buf.is_empty(), "{version}: trailing bytes");
			assert!(!msg.forward, "{version}");
			assert!(msg.range_filters, "{version}");
			assert!(!msg.properties_wanted, "{version}");
		}
	}

	/// INCLUDE_PROPERTIES is a uint8, one raw byte with no Length. Reading a Length would
	/// swallow the parameter after it, or fail on the value.
	#[test]
	fn include_properties_is_a_uint8() {
		let msg = Subscribe {
			request_id: RequestId(1),
			track_namespace: crate::Path::new("live"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::Unfiltered,
			fill: None,
			properties_wanted: false,
			forward: true,
			range_filters: false,
		};

		#[rustfmt::skip]
		let expected = subscribe_body(&[
			0x04, // Number of Parameters
			0x10, 0x01, // FORWARD = 1
			0x10, 0x80, // SUBSCRIBER_PRIORITY (0x20) = 128, a raw byte
			0x02, 0x02, // GROUP_ORDER (0x22) = Descending
			0x13, 0x00, // INCLUDE_PROPERTIES (0x35) = 0
		]);
		assert_eq!(encode_message(&msg, Version::Draft20), expected);

		// Anything but 0 or 1 is a protocol violation.
		let body = subscribe_body(&[0x01, 0x35, 0x02]);
		assert!(decode_message::<Subscribe>(&body, Version::Draft20).is_err());
	}

	/// FORWARD=0 is legal on every draft. It decodes so the request can be refused,
	/// rather than closing the session.
	#[test]
	fn subscribe_decodes_forward_zero() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
		] {
			let msg = Subscribe {
				request_id: RequestId(1),
				track_namespace: crate::Path::new("live"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: GroupOrder::Descending,
				filter: Filter::NextObject,
				fill: None,
				properties_wanted: true,
				forward: false,
				range_filters: false,
			};
			let decoded: Subscribe = decode_message(&encode_message(&msg, version), version).unwrap();
			assert!(!decoded.forward, "{version}");
		}
	}

	/// AUTHORIZATION TOKEN is a Key-Value-Pair with an odd type through draft-16, so its
	/// value is length prefixed there too.
	#[test]
	fn subscribe_accepts_a_token_on_every_draft() {
		#[rustfmt::skip]
		let body = subscribe_body(&[
			0x01, // Number of Parameters
			0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
		]);
		for version in [
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
		] {
			let mut body = body.clone();
			if version == Version::Draft17 {
				// Required Request ID Delta, draft-17 only.
				body.insert(1, 0x00);
			}
			let wire = body;
			let mut buf = Decoder::new(&wire, version.into());
			Subscribe::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
			assert!(buf.is_empty(), "{version}: trailing bytes");
		}
	}

	/// Draft-15 ignores a parameter it does not know. From draft-16 on, a parameter from
	/// a later draft is unknown, which closes the session.
	#[test]
	fn subscribe_ignores_unrecognized_parameters_in_draft15() {
		let body = subscribe_body(&[0x01, 0x32, 0x00]);
		decode_message::<Subscribe>(&body, Version::Draft15).expect("draft-15 ignores NEW_GROUP_REQUEST");
	}

	/// A parameter from a later draft is still an unknown parameter, which the draft makes
	/// a protocol violation.
	#[test]
	fn subscribe_refuses_parameters_before_their_draft() {
		for (version, params) in [
			// SUBGROUP_DELIVERY_TIMEOUT arrived in draft-18, and 0x06 is not a draft-16 id.
			(Version::Draft16, &[0x01, 0x06, 0x00][..]),
			// The Range Filters arrived in draft-19.
			(Version::Draft18, &[0x01, 0x25, 0x00][..]),
			// INCLUDE_PROPERTIES arrived in draft-20.
			(Version::Draft19, &[0x01, 0x35, 0x00][..]),
		] {
			let body = subscribe_body(params);
			assert!(
				decode_message::<Subscribe>(&body, version).is_err(),
				"{version}: {params:x?}"
			);
		}
	}

	/// REQUEST_UPDATE (draft-20 Figure 12) carries a token, NEW_GROUP_REQUEST and the
	/// Range Filters, TRACK_PROPERTY_FILTER included, since the update may be for a
	/// SUBSCRIBE_TRACKS.
	#[test]
	fn request_update_decodes_every_draft20_parameter() {
		#[rustfmt::skip]
		let body = [
			0x02, // Request ID
			0x04, // Number of Parameters
			0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
			0x22, 0x00, // SUBGROUP_FILTER (0x25), empty: remove the filter
			0x04, 0x04, 0x00, 0x02, 0x00, 0x01, // TRACK_PROPERTY_FILTER (0x29): SetID 0, type 2, range 0-1
			0x09, 0x00, // NEW_GROUP_REQUEST (0x32)
		];
		let decoded: SubscribeUpdate = decode_message(&body, Version::Draft20).unwrap();
		assert_eq!(decoded.request_id, RequestId(2));
	}

	/// SUBGROUP_DELIVERY_TIMEOUT (0x06) is not a draft-16 parameter. Draft-18 defines it
	/// for SUBSCRIBE, and the value is dropped.
	#[test]
	fn subgroup_delivery_timeout_follows_its_draft() {
		let head: &[u8] = &[
			0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x05, b'v', b'i', b'd', b'e', b'o',
		];
		let body = [head, &[0x01, 0x06, 0x00][..]].concat();
		assert!(decode_message::<Subscribe>(&body, Version::Draft16).is_err());
		decode_message::<Subscribe>(&body, Version::Draft18).expect("draft-18 accepts SUBGROUP_DELIVERY_TIMEOUT");

		// GROUP_ORDER (0x22) is known in draft-16 but not defined for REQUEST_UPDATE.
		// Request ID, Subscription Request ID, one parameter, absolute key, varint value.
		let update16: &[u8] = &[0x02, 0x0A, 0x01, 0x22, 0x02];
		decode_message::<SubscribeUpdate>(update16, Version::Draft16).expect("draft-16 ignores GROUP_ORDER");
		// Draft-18 has no Subscription Request ID. The value is a uint8.
		let update18: &[u8] = &[0x02, 0x01, 0x22, 0x02];
		assert!(decode_message::<SubscribeUpdate>(update18, Version::Draft18).is_err());
	}
}

#[cfg(test)]
mod cache_duration_tests {
	use super::*;
	use std::time::Duration;

	#[test]
	fn max_cache_duration_is_read_in_each_drafts_field() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			for age in [0u64, 30_000] {
				let mut payload = match version {
					Version::Draft14 => vec![0, 0, 0, 1, 0, 1, 4],
					Version::Draft15 => vec![0, 0, 1, 4],
					Version::Draft16 => vec![0, 0, 0, 4],
					_ => vec![0, 0, 4],
				};
				Encoder::new(&mut payload, version.into()).varint(age).unwrap();
				let got = crate::coding::decode_buf(&mut payload.as_slice(), version, SubscribeOk::decode_msg).unwrap();
				assert_eq!(
					got.properties.max_cache_duration,
					Some(Duration::from_millis(age)),
					"{version}"
				);
			}
		}
	}

	#[test]
	fn legacy_never_sends_cache_duration_and_modern_preserves_optional_value() {
		for version in [
			Version::Draft14,
			Version::Draft15,
			Version::Draft16,
			Version::Draft17,
			Version::Draft18,
			Version::Draft19,
			Version::Draft20,
			Version::Draft21,
			Version::Draft22,
		] {
			let legacy = matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16);
			for age in [None, Some(Duration::ZERO), Some(Duration::from_secs(30))] {
				let ok = SubscribeOk {
					request_id: legacy.then_some(RequestId(0)),
					track_alias: 0,
					largest: None,
					properties: Properties {
						max_cache_duration: age,
						..Default::default()
					},
				};
				let mut payload = Vec::new();
				ok.encode_msg(&mut Encoder::new(&mut payload, version.into()), version)
					.unwrap();
				let got = crate::coding::decode_buf(&mut payload.as_slice(), version, SubscribeOk::decode_msg).unwrap();
				assert_eq!(
					got.properties.max_cache_duration,
					if legacy { None } else { age },
					"{version}"
				);
			}
		}
	}
}
