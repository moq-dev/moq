//! IETF moq-transport subscribe messages

use std::borrow::Cow;

use crate::{
	Path,
	coding::*,
	ietf::{Fill, Filter, GroupOrder, Location, Opaque, Properties, RequestId},
};

use super::token::RequestToken;

use super::Message;
use super::epoch;
use super::namespace::{decode_track, encode_track};

use super::Version;

/// Subscribe message (0x03)
/// Sent by the subscriber to request all future objects for the given track.
#[derive(Clone, Debug)]
pub struct Subscribe<'a> {
	pub request_id: RequestId,
	pub track_namespace: Path,
	pub track_name: Cow<'a, str>,
	pub subscriber_priority: u8,
	pub group_order: GroupOrder,
	/// Which Objects the subscription delivers.
	pub filter: Filter,
	/// The draft-22 backfill request, if the subscriber asked for one.
	pub fill: Option<Fill>,
	/// Whether the subscriber wants Track Properties on the response (draft-22).
	pub properties_wanted: bool,
	/// Whether Objects are forwarded (FORWARD); a paused subscription forwards none until resumed.
	pub forward: bool,
	/// Whether RENDEZVOUS_TIMEOUT asks to wait for a missing publisher; never sent.
	pub rendezvous: bool,
	/// Whether the request carried a Range Filter (0x25-0x28). We advertise no
	/// MAX_FILTER_RANGES, so the request is refused rather than served unfiltered.
	/// Never encoded; we send no range filters.
	pub range_filters: bool,
	/// The `AUTHORIZATION TOKEN` (0x03) this request presents. At most one is
	/// accepted: decoding refuses a repeated or second token.
	pub authorization_token: Option<RequestToken>,
	/// NEW_GROUP_REQUEST (0x32): the subscriber's largest known Group ID plus one, or 0
	/// when it knows none.
	pub new_group: Option<u64>,
	/// Subscriber max age in milliseconds (MoQ Timestamp), if negotiated.
	pub max_age: Option<u64>,
	/// The publisher instance the subscriber expects (MoQ Epoch). Only on a
	/// session that negotiated it; the session refuses one that arrives otherwise.
	pub epoch: Option<crate::Epoch>,
}

impl Subscribe<'_> {
	/// Copy the borrowed track name, so the request outlives the bytes it came from.
	pub fn into_owned(self) -> Subscribe<'static> {
		Subscribe {
			request_id: self.request_id,
			track_namespace: self.track_namespace,
			track_name: Cow::Owned(self.track_name.into_owned()),
			subscriber_priority: self.subscriber_priority,
			group_order: self.group_order,
			filter: self.filter,
			fill: self.fill,
			properties_wanted: self.properties_wanted,
			forward: self.forward,
			rendezvous: self.rendezvous,
			range_filters: self.range_filters,
			authorization_token: self.authorization_token,
			new_group: self.new_group,
			max_age: self.max_age,
			epoch: self.epoch,
		}
	}
}

/// The Range Filters (0x25-0x29) arrived in draft-22.
pub(super) fn has_range_filters(version: Version) -> bool {
	!matches!(version, Version::Draft18)
}

impl<'a> Message<'a> for Subscribe<'a> {
	const ID: u64 = 0x03;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let (track_namespace, track_name) = decode_track(r)?;
		let track_name = Cow::Borrowed(track_name);

		decode_params!(r, version,
			0x02 => _object_delivery_timeout: Option<u64>,
			0x03 => authorization_token: Vec<super::token::RequestToken>,
			0x04 => rendezvous_timeout: Option<u64>,
			0x06 => _subgroup_delivery_timeout: Option<u64>,
			0x10 => forward: Option<bool>,
			0x20 => subscriber_priority: Option<u8>,
			0x21 => filter: Option<Filter>,
			0x22 => group_order: Option<GroupOrder>,
			0x23 => fill: Option<Fill> where Filter::is_draft20(version),
			0x25 => subgroup_filter: Vec<Opaque> where has_range_filters(version),
			0x26 => object_id_filter: Vec<Opaque> where has_range_filters(version),
			0x27 => priority_filter: Vec<Opaque> where has_range_filters(version),
			0x28 => object_property_filter: Vec<Opaque> where has_range_filters(version),
			0x32 => new_group: Option<u64>,
			0x35 => include_properties: Option<bool> where Filter::is_draft20(version),
			epoch::EPOCH_PARAM => epoch: Option<crate::Epoch>,
			super::max_age::PARAMETER => max_age: Option<u64>,
		);
		let authorization_token = super::token::single(authorization_token)?;

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

		// A zero cap still distinguishes a requested wait from an immediate miss.
		let rendezvous = rendezvous_timeout.is_some_and(|millis| millis > 0);

		let subscriber_priority = subscriber_priority.unwrap_or(super::priority::DEFAULT_WIRE);
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
			rendezvous,
			range_filters,
			authorization_token,
			new_group,
			max_age: super::max_age::validate(max_age)?,
			epoch,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_track(w, &self.track_namespace, &self.track_name)?;

		// FILL_PARAMETERS arrived in draft-22. Sending it to an older peer would be an
		// unknown parameter, which is a protocol violation, so it is dropped instead.
		// A subscriber there simply joins mid-group, which is what it did all along.
		let fill = self.fill.filter(|_| Filter::is_draft20(version));

		// INCLUDE_PROPERTIES defaults to 1, so only the opt-out is worth bytes. It
		// arrived in draft-22, and an older peer would read it as an unknown
		// parameter, which is a protocol violation.
		let include_properties = (!self.properties_wanted && Filter::is_draft20(version)).then_some(false);
		// Only a session that negotiated MoQ Epoch names one.
		let epoch = self.epoch.clone();

		encode_params!(w, version,
			0x03 => self.authorization_token.clone(),
			0x10 => self.forward,
			0x20 => self.subscriber_priority,
			0x21 => self.filter,
			0x22 => self.group_order,
			0x23 => fill,
			0x32 => self.new_group,
			0x35 => include_properties,
			epoch::EPOCH_PARAM => epoch,
			super::max_age::PARAMETER => self.max_age,
		);

		Ok(())
	}
}

/// SubscribeOk message (0x04)
#[derive(Clone, Debug)]
pub struct SubscribeOk {
	pub track_alias: u64,

	/// The largest Location in the track (LARGEST_OBJECT, 0x09), which the spec requires
	/// once the track has content. It is what a subscriber sizes a fill against.
	pub largest: Option<Location>,

	/// Metadata about the track, sent as Track Properties.
	pub properties: Properties,
}

impl<'a> Message<'a> for SubscribeOk {
	const ID: u64 = 0x04;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.varint(self.track_alias)?;

		// GROUP_ORDER is not a legal SUBSCRIBE_OK parameter; a peer closes the session with
		// PROTOCOL_VIOLATION when it sees one. The publisher's preference is a
		// DEFAULT_PUBLISHER_GROUP_ORDER track property instead.
		encode_params!(w, version,
			0x09 => self.largest,
		);

		// Track Properties are the final field, so nothing may follow.
		self.properties.encode(w, version)?;

		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let track_alias = r.varint()?;

		// GROUP_ORDER closes the session. LARGEST_OBJECT is required once the track has
		// content, so rejecting it would tear down a session over a parameter compliant
		// publishers must send. EXPIRES is ignored.
		decode_params!(r, version,
			0x08 => _expires: Option<u64>,
			0x09 => largest: Option<Location>,
		);
		let properties = Properties::decode(r, version)?;

		Ok(Self {
			track_alias,
			largest,
			properties,
		})
	}
}

/// SubscribeUpdate message (0x02)
#[derive(Clone, Debug)]
pub struct SubscribeUpdate {
	pub request_id: RequestId,
	/// `None` leaves the priority as it is.
	pub subscriber_priority: Option<u8>,
	/// `None` leaves forwarding as it is, as for the priority.
	pub forward: Option<bool>,
	/// The new LOCATION_FILTER; `None` keeps the subscription's own range.
	pub filter: Option<Filter>,
	/// The `AUTHORIZATION TOKEN` (0x03) renewing the request's credential, if any. At
	/// most one is accepted: decoding refuses a repeated or second token.
	pub authorization_token: Option<RequestToken>,
	/// NEW_GROUP_REQUEST (0x32), as on [`Subscribe::new_group`].
	pub new_group: Option<u64>,
	/// Subscriber max age in milliseconds (MoQ Timestamp), if negotiated.
	pub max_age: Option<u64>,
	/// FILL_PARAMETERS (0x23, draft-22): the backfill requested with this update.
	pub fill: Option<Fill>,
}

impl<'a> Message<'a> for SubscribeUpdate {
	const ID: u64 = 0x02;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		// REQUEST_UPDATE
		self.request_id.encode(w, version)?;
		encode_params!(w, version,
			0x03 => self.authorization_token.clone(),
			0x10 => self.forward,
			0x20 => self.subscriber_priority,
			0x21 => self.filter,
			0x23 => self.fill.filter(|_| Filter::is_draft20(version)),
			0x32 => self.new_group,
			super::max_age::PARAMETER => self.max_age,
		);

		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		// REQUEST_UPDATE
		let request_id = RequestId::decode(r, version)?;
		// Nothing reads an update's Range Filters yet; they are
		// consumed so a legal update does not fail the session.
		decode_params!(r, version,
			0x02 => _object_delivery_timeout: Option<u64>,
			0x03 => authorization_token: Vec<RequestToken>,
			0x06 => _subgroup_delivery_timeout: Option<u64>,
			0x10 => forward: Option<bool>,
			0x20 => subscriber_priority: Option<u8>,
			0x21 => filter: Option<Filter>,
			0x23 => fill: Option<Fill> where Filter::is_draft20(version),
			0x25 => _subgroup_filter: Vec<Opaque> where has_range_filters(version),
			0x26 => _object_id_filter: Vec<Opaque> where has_range_filters(version),
			0x27 => _priority_filter: Vec<Opaque> where has_range_filters(version),
			0x28 => _object_property_filter: Vec<Opaque> where has_range_filters(version),
			0x29 => _track_property_filter: Vec<Opaque> where has_range_filters(version),
			0x32 => new_group: Option<u64>,
			super::max_age::PARAMETER => max_age: Option<u64>,
		);

		Ok(Self {
			request_id,
			subscriber_priority,
			forward,
			filter,
			authorization_token: super::token::single(authorization_token)?,
			new_group,
			max_age: super::max_age::validate(max_age)?,
			fill,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

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

	#[test]
	fn subscriber_max_age_round_trips() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for max_age in [None, Some(0), Some(250), Some(super::super::max_age::MAX_MILLIS)] {
				let msg = Subscribe {
					max_age,
					request_id: RequestId(0),
					track_namespace: Path::from_static("live"),
					track_name: "catalog".into(),
					subscriber_priority: 128,
					group_order: GroupOrder::Descending,
					filter: Filter::Unfiltered,
					fill: None,
					properties_wanted: true,
					forward: true,
					rendezvous: false,
					range_filters: false,
					authorization_token: None,
					new_group: None,
					epoch: None,
				};
				let wire = encode_message(&msg, version);

				let decoded: Subscribe = decode_message(&wire, version).unwrap();
				assert_eq!(decoded.max_age, max_age, "{version}");
				let update = SubscribeUpdate {
					max_age,
					request_id: RequestId(2),
					subscriber_priority: None,
					forward: None,
					filter: None,
					authorization_token: None,
					new_group: None,
					fill: None,
				};
				let wire = encode_message(&update, version);

				let decoded: SubscribeUpdate = decode_message(&wire, version).unwrap();
				assert_eq!(decoded.max_age, max_age, "{version}");
			}
		}
	}

	#[test]
	fn test_subscribe_round_trip() {
		let msg = Subscribe {
			new_group: None,
			max_age: None,
			request_id: RequestId(1),
			track_namespace: Path::from_static("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.track_namespace.as_str(), "test");
		assert_eq!(decoded.track_name, "video");
		assert_eq!(decoded.subscriber_priority, 128);
	}

	/// Build a SUBSCRIBE body carrying a single RENDEZVOUS_TIMEOUT parameter.
	const VERSIONS: [Version; 3] = [Version::Draft18, Version::Draft20, Version::Draft22];

	/// NEW_GROUP_REQUEST survives SUBSCRIBE and REQUEST_UPDATE.
	#[test]
	fn new_group_request_round_trips() {
		for version in VERSIONS {
			let expected = Some(7);
			let subscribe = Subscribe {
				new_group: Some(7),
				max_age: None,
				request_id: RequestId(1),
				track_namespace: Path::from_static("test"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: GroupOrder::Descending,
				filter: Filter::NextObject,
				fill: None,
				properties_wanted: true,
				forward: true,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
			};
			let wire = encode_message(&subscribe, version);

			let decoded: Subscribe = decode_message(&wire, version).unwrap();
			assert_eq!(decoded.new_group, expected, "{version}: SUBSCRIBE");

			let update = SubscribeUpdate {
				new_group: Some(7),
				max_age: None,
				request_id: RequestId(3),
				subscriber_priority: None,
				forward: None,
				filter: None,
				authorization_token: None,
				fill: None,
			};
			let wire = encode_message(&update, version);

			let decoded: SubscribeUpdate = decode_message(&wire, version).unwrap();
			assert_eq!(decoded.new_group, expected, "{version}: REQUEST_UPDATE");
		}
	}

	fn subscribe_with_rendezvous(millis: u64, version: Version) -> Vec<u8> {
		fn build(millis: u64, version: Version) -> Result<Vec<u8>, EncodeError> {
			let mut buf = Vec::new();
			let w = &mut Encoder::new(&mut buf, version.into());
			RequestId(1).encode(w, version)?;
			encode_track(w, &Path::from_static("test"), "video")?;
			encode_params!(w, version,
				0x04 => millis,
			);
			Ok(buf)
		}

		build(millis, version).unwrap()
	}

	/// A subscriber may send RENDEZVOUS_TIMEOUT. We do not honor the wait, but an
	/// unknown message parameter is a session-killing protocol violation, so it still has to
	/// parse: failing to decode it takes the whole session down instead of answering the
	/// SUBSCRIBE.
	#[test]
	fn rendezvous_timeout_retains_whether_a_wait_was_requested() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for millis in [0, 5000] {
				let encoded = subscribe_with_rendezvous(millis, version);
				let msg: Subscribe = decode_message(&encoded, version)
					.unwrap_or_else(|err| panic!("{version} rendezvous {millis}ms: {err}"));
				assert_eq!(msg.track_name, "video");
				assert_eq!(msg.rendezvous, millis > 0);
			}
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
		let r = &mut Decoder::new(encoded, version.into());
		RequestId::decode(r, version).unwrap();
		decode_track(r).unwrap();

		// Keys are delta coded, but the first is absolute.
		let count = r.varint().unwrap();
		(count > 0).then(|| r.varint().unwrap())
	}

	/// We never ask a peer to hold a subscription open, so the parameter stays off our wire.
	#[test]
	fn rendezvous_timeout_is_never_sent() {
		let msg = Subscribe {
			new_group: None,
			max_age: None,
			request_id: RequestId(1),
			track_namespace: Path::from_static("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
		};

		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
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
			new_group: None,
			max_age: None,
			request_id: RequestId(100),
			track_namespace: Path::from_static("conference/room123"),
			track_name: "audio".into(),
			subscriber_priority: 255,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: Subscribe = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.track_namespace.as_str(), "conference/room123");
	}

	#[test]
	fn test_subscribe_ok() {
		let msg = SubscribeOk {
			track_alias: 42,
			largest: None,
			properties: Properties::default(),
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.track_alias, 42);
	}

	/// The parameter value is two bare varints. The empty Track Properties block follows the
	/// parameters.
	#[test]
	fn test_subscribe_ok_accepts_largest_object() {
		let payload = [0x04, 0x01, 0x09, 0x05, 0x00];
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let decoded: SubscribeOk = decode_message(&payload, version).unwrap();

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
	/// it carries it as a bare Location parameter.
	#[test]
	fn test_subscribe_ok_largest_object_wire_vectors() {
		let largest = Some(Location { group: 9, object: 3 });

		for (version, expected) in [
			(Version::Draft18, &[42, 1, 0x09, 0x09, 0x03][..]),
			(Version::Draft22, &[42, 1, 0x09, 0x09, 0x03][..]),
		] {
			let msg = SubscribeOk {
				track_alias: 42,
				largest,
				properties: Properties::default(),
			};

			assert_eq!(encode_message(&msg, version), expected, "{version}");

			let decoded: SubscribeOk = decode_message(expected, version).unwrap();
			assert_eq!(decoded.largest, largest, "{version}");
		}
	}

	/// Every parameter the draft lets a message carry has to parse, even the ones we act on
	/// nowhere: an unlisted key fails the whole message, which tears the session down
	/// instead of letting the request reach its answer.
	#[test]
	fn subscribe_accepts_the_delivery_timeouts() -> Result<(), EncodeError> {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut body = Vec::new();
			let w = &mut Encoder::new(&mut body, version.into());
			RequestId(1).encode(w, version)?;
			encode_track(w, &crate::Path::from_static("broadcast"), "video")?;
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
			(false, Version::Draft22),
			(true, Version::Draft22),
			// Older drafts have no such parameter, so the opt-out is dropped rather than
			// sent as one they would treat as a protocol violation.
			(true, Version::Draft18),
		] {
			let msg = Subscribe {
				new_group: None,
				max_age: None,
				request_id: RequestId(1),
				track_namespace: crate::Path::from_static("broadcast"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: GroupOrder::Descending,
				filter: Filter::NextObject,
				fill: None,
				properties_wanted: wanted,
				forward: true,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
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

		let result: Result<Subscribe, _> = decode_message(&invalid_bytes, Version::Draft18);
		assert!(result.is_err());
	}

	/// Moxygen's interop client sends filter type 250 by default; only 0x1-0x4 are registered.
	#[test]
	fn test_subscribe_rejects_unregistered_filter_type() {
		let subscribe = |filter_type: &[u8]| {
			let mut bytes = vec![0x00]; // request_id
			bytes.extend_from_slice(b"\x01\x0bnonexistent\x0dno-such-track"); // namespace, name
			bytes.extend_from_slice(&[0x01, 0x21, filter_type.len() as u8]); // one SUBSCRIPTION_FILTER
			bytes.extend_from_slice(filter_type);
			decode_message::<Subscribe>(&bytes, Version::Draft18).map(|msg| msg.filter)
		};

		assert!(matches!(subscribe(&[0x80, 0xfa]), Err(DecodeError::InvalidValue)));
		assert_eq!(subscribe(&[0x02]).unwrap(), Filter::NextObject);
	}

	/// SUBSCRIBER_PRIORITY is a parameter whose omission means 128, not this
	/// stack's unset 127, so a relay forwards it unchanged.
	#[test]
	fn test_subscribe_omitted_priority_is_128() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut bytes = vec![0x00]; // request_id
			bytes.extend_from_slice(b"\x01\x0bnonexistent\x0dno-such-track"); // namespace, name
			bytes.push(0x00); // no parameters
			let msg = decode_message::<Subscribe>(&bytes, version).unwrap();
			assert_eq!(msg.subscriber_priority, 128, "{version}");
		}
	}

	#[test]
	fn test_subscribe_v18_round_trip() {
		let msg = Subscribe {
			new_group: None,
			max_age: None,
			request_id: RequestId(1),
			track_namespace: Path::from_static("test"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
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
			track_alias: 42,
			largest: None,
			properties: Properties::default(),
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: SubscribeOk = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.track_alias, 42);
	}

	/// The registered priority property is two literal bytes after the message body. The
	/// decoder returns the same wire value.
	#[test]
	fn subscribe_ok_priority_property_bytes_on_every_draft() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let mut msg = SubscribeOk {
				track_alias: 42,
				largest: None,
				properties: Properties::default(),
			};
			let baseline = encode_message(&msg, version);
			msg.properties.priority = Some(37);
			let encoded = encode_message(&msg, version);
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

	/// GROUP_ORDER (0x22) is not a legal SUBSCRIBE_OK *message parameter*; a peer closes the
	/// session with PROTOCOL_VIOLATION when it sees one. The publisher's preference
	/// belongs in the DEFAULT_PUBLISHER_GROUP_ORDER track property,
	/// which shares the number 0x22 in the separate property registry.
	#[test]
	fn test_subscribe_ok_v18_group_order_is_a_property() {
		let msg = SubscribeOk {
			track_alias: 42,
			largest: None,
			properties: Properties {
				unknown: Default::default(),
				dynamic_groups: false,
				compress: false,
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

	/// Draft-18 closes the session when a known parameter shows up on a message that does
	/// not define it, such as GROUP_ORDER on SUBSCRIBE_OK.
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

	/// REQUEST_UPDATE has no `required_request_id_delta` field (moq-wg/moq-transport#1615).
	#[test]
	fn test_subscribe_update_v18_round_trip() {
		let msg = SubscribeUpdate {
			new_group: None,
			max_age: None,
			request_id: RequestId(10),
			subscriber_priority: Some(200),
			forward: Some(true),
			filter: Some(Filter::NextObject),
			authorization_token: None,
			fill: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: SubscribeUpdate = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(10));
		assert_eq!(decoded.subscriber_priority, Some(200));
		assert_eq!(decoded.forward, Some(true));
	}

	/// The head of a SUBSCRIBE (draft-22 Figure 11) up to its Number of Parameters:
	/// Request ID 1, Track Namespace ("live"), Track Name ("video"). Every value is
	/// below 128, so each varint is one byte in the drafts before and after 17 alike.
	const SUBSCRIBE_HEAD: &[u8] = &[
		0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x05, b'v', b'i', b'd', b'e', b'o',
	];

	fn subscribe_body(params: &[u8]) -> Vec<u8> {
		[SUBSCRIBE_HEAD, params].concat()
	}

	/// Every parameter draft-22 lets a SUBSCRIBE carry that we don't act on still decodes,
	/// with what we have to refuse recorded rather than failing the session. A second distinct
	/// token, which the draft allows, refuses only the request.
	#[test]
	fn subscribe_decodes_every_draft22_parameter() {
		let second: &[u8] = &[0x00, 0x03, 0x03, 0x00, 0xBB];
		#[rustfmt::skip]
		let body = |second: &[u8]| subscribe_body(&<[&[u8]]>::concat(&[
			&[0x05 + u8::from(!second.is_empty())], // Number of Parameters
			&[0x03, 0x03, 0x03, 0x00, 0xAA], // AUTHORIZATION TOKEN: USE_VALUE, type 0, one byte
			second, // a second, distinct one, which the draft allows
			&[0x0D, 0x00], // FORWARD (0x10) = 0, a uint8
			&[0x15, 0x03, 0x00, 0x00, 0x01], // SUBGROUP_FILTER (0x25): SetID 0, range 0-1
			&[0x0D, 0x07], // NEW_GROUP_REQUEST (0x32), a varint
			&[0x03, 0x00], // INCLUDE_PROPERTIES (0x35) = 0, a uint8
		]));

		let version = Version::Draft22;
		let wire = body(&[]);
		let mut buf = Decoder::new(&wire, version.into());
		let msg = Subscribe::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
		assert!(buf.is_empty(), "{version}: trailing bytes");
		assert!(msg.authorization_token.is_some(), "{version}");
		assert!(!msg.forward, "{version}");
		assert!(msg.range_filters, "{version}");
		assert!(!msg.properties_wanted, "{version}");

		let wire = body(second);
		let mut buf = Decoder::new(&wire, version.into());
		assert!(
			matches!(
				Subscribe::decode_msg(&mut buf, version),
				Err(DecodeError::MultipleAuthTokens)
			),
			"{version}"
		);
	}

	/// INCLUDE_PROPERTIES is a uint8, one raw byte with no Length. Reading a Length would
	/// swallow the parameter after it, or fail on the value.
	#[test]
	fn include_properties_is_a_uint8() {
		let msg = Subscribe {
			new_group: None,
			max_age: None,
			request_id: RequestId(1),
			track_namespace: crate::Path::from_static("live"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::Unfiltered,
			fill: None,
			properties_wanted: false,
			forward: true,
			rendezvous: false,
			range_filters: false,
			authorization_token: None,
			epoch: None,
		};

		#[rustfmt::skip]
		let expected = subscribe_body(&[
			0x04, // Number of Parameters
			0x10, 0x01, // FORWARD = 1
			0x10, 0x80, // SUBSCRIBER_PRIORITY (0x20) = 128, a raw byte
			0x02, 0x02, // GROUP_ORDER (0x22) = Descending
			0x13, 0x00, // INCLUDE_PROPERTIES (0x35) = 0
		]);
		assert_eq!(encode_message(&msg, Version::Draft22), expected);

		// Anything but 0 or 1 is a protocol violation.
		let body = subscribe_body(&[0x01, 0x35, 0x02]);
		assert!(decode_message::<Subscribe>(&body, Version::Draft22).is_err());
	}

	/// Draft-22's LOCATION_FILTER is a Location Filter Type with no Length, so Next Object
	/// is the single byte 0x05 and the parameter after it starts right behind it.
	#[test]
	fn subscribe_decodes_a_draft22_next_object() {
		#[rustfmt::skip]
		let body = subscribe_body(&[
			0x04, // Number of Parameters
			0x10, 0x01, // FORWARD (0x10) = 1
			0x10, 0x40, // SUBSCRIBER_PRIORITY (0x20) = 64
			0x01, 0x05, // LOCATION_FILTER (0x21): Next Object
			0x01, 0x01, // GROUP_ORDER (0x22) = Ascending
		]);

		let msg: Subscribe = decode_message(&body, Version::Draft22).unwrap();
		assert_eq!(msg.filter, Filter::NextObject);
		assert_eq!(msg.subscriber_priority, 64);
		assert_eq!(msg.group_order, GroupOrder::Ascending);
		assert_eq!(encode_message(&msg, Version::Draft22), body);
	}

	/// FORWARD=0 is legal on every draft, a paused request rather than a session error.
	#[test]
	fn subscribe_decodes_forward_zero() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let msg = Subscribe {
				new_group: None,
				max_age: None,
				request_id: RequestId(1),
				track_namespace: crate::Path::from_static("live"),
				track_name: "video".into(),
				subscriber_priority: 128,
				group_order: GroupOrder::Descending,
				filter: Filter::NextObject,
				fill: None,
				properties_wanted: true,
				forward: false,
				rendezvous: false,
				range_filters: false,
				authorization_token: None,
				epoch: None,
			};
			let wire = encode_message(&msg, version);

			let decoded: Subscribe = decode_message(&wire, version).unwrap();
			assert!(!decoded.forward, "{version}");
		}
	}

	/// AUTHORIZATION TOKEN is a Key-Value-Pair with an odd type, so its value is length
	/// prefixed.
	#[test]
	fn subscribe_accepts_a_token_on_every_draft() {
		#[rustfmt::skip]
		let body = subscribe_body(&[
			0x01, // Number of Parameters
			0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
		]);
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let wire = body.clone();
			let mut buf = Decoder::new(&wire, version.into());
			Subscribe::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
			assert!(buf.is_empty(), "{version}: trailing bytes");
		}
	}

	/// A parameter from a later draft is still an unknown parameter, which the draft makes
	/// a protocol violation.
	#[test]
	fn subscribe_refuses_parameters_before_their_draft() {
		for (version, params) in [
			// The Range Filters arrived in draft-22.
			(Version::Draft18, &[0x01, 0x25, 0x00][..]),
			// INCLUDE_PROPERTIES arrived in draft-22.
			(Version::Draft18, &[0x01, 0x35, 0x00][..]),
		] {
			let body = subscribe_body(params);
			assert!(
				decode_message::<Subscribe>(&body, version).is_err(),
				"{version}: {params:x?}"
			);
		}
	}

	/// REQUEST_UPDATE (draft-22 Figure 10) carries a token, NEW_GROUP_REQUEST and the
	/// Range Filters, TRACK_PROPERTY_FILTER included, since the update may be for a
	/// SUBSCRIBE_TRACKS.
	#[test]
	fn request_update_decodes_every_draft22_parameter() {
		#[rustfmt::skip]
		let body = [
			0x02, // Request ID
			0x04, // Number of Parameters
			0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
			0x22, 0x00, // SUBGROUP_FILTER (0x25), empty: remove the filter
			0x04, 0x04, 0x00, 0x02, 0x00, 0x01, // TRACK_PROPERTY_FILTER (0x29): SetID 0, type 2, range 0-1
			0x09, 0x00, // NEW_GROUP_REQUEST (0x32)
		];
		let decoded: SubscribeUpdate = decode_message(&body, Version::Draft22).unwrap();
		assert_eq!(decoded.request_id, RequestId(2));
	}

	/// Draft-18 defines SUBGROUP_DELIVERY_TIMEOUT (0x06) for SUBSCRIBE, and the value is
	/// dropped.
	#[test]
	fn subgroup_delivery_timeout_follows_its_draft() {
		let head: &[u8] = &[
			0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x05, b'v', b'i', b'd', b'e', b'o',
		];
		let body = [head, &[0x01, 0x06, 0x00][..]].concat();
		decode_message::<Subscribe>(&body, Version::Draft18).expect("draft-18 accepts SUBGROUP_DELIVERY_TIMEOUT");

		// GROUP_ORDER (0x22) is not defined for REQUEST_UPDATE.
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
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for age in [0u64, 30_000] {
				let mut payload = vec![0, 0, 4];
				Encoder::new(&mut payload, version.into()).varint(age).unwrap();
				let got =
					crate::coding::decode_buf(&mut payload.as_slice(), version, |r, v| SubscribeOk::decode_msg(r, v))
						.unwrap();
				assert_eq!(
					got.properties.max_cache_duration,
					Some(Duration::from_millis(age)),
					"{version}"
				);
			}
		}
	}

	#[test]
	fn cache_duration_preserves_optional_value() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for age in [None, Some(Duration::ZERO), Some(Duration::from_secs(30))] {
				let ok = SubscribeOk {
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
				let got =
					crate::coding::decode_buf(&mut payload.as_slice(), version, |r, v| SubscribeOk::decode_msg(r, v))
						.unwrap();
				assert_eq!(got.properties.max_cache_duration, age, "{version}");
			}
		}
	}
}
