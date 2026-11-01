//! IETF moq-transport SUBSCRIBE_TRACKS (draft-18+) and the PUBLISH_SKIPPED answering it.

use std::borrow::Cow;

use crate::{
	Path,
	coding::*,
	ietf::{Filter, GroupOrder, Opaque, RequestId},
};

use super::Message;
use super::Version;
use super::namespace::{decode_namespace, encode_namespace};
use super::subscribe::has_range_filters;
use super::token::RequestToken;

/// SUBSCRIBE_TRACKS (0x51, draft-18+): list every track under a namespace prefix, each
/// answered with a PUBLISH (or a [`PublishSkipped`]) as it appears.
///
/// We send it to list an IETF peer's tracks, and refuse one we receive, so only what our
/// request carries is kept; the rest is read to validate it and dropped.
#[derive(Clone, Debug)]
pub struct SubscribeTracks {
	pub request_id: RequestId,
	pub prefix: Path,
	/// FORWARD: absent means 1, which asks each PUBLISH to push its objects at once.
	pub forward: bool,
	/// The default group order of each PUBLISH subscription.
	pub group_order: Option<GroupOrder>,
	/// INCLUDE_PROPERTIES (draft-22): whether each PUBLISH carries Track Properties.
	pub properties_wanted: bool,
	/// The `AUTHORIZATION TOKEN` (0x03) this request presents.
	pub authorization_token: Option<RequestToken>,
}

impl<'a> Message<'a> for SubscribeTracks {
	const ID: u64 = 0x51;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_namespace(w, &self.prefix)?;
		// Absent means 1, so only a paused listing writes it.
		let include_properties = (!self.properties_wanted && Filter::is_draft20(version)).then_some(false);
		// Draft-18 allows only AUTHORIZATION_TOKEN and FORWARD here. Draft-20 and draft-22
		// (section 9.18) also list GROUP_ORDER.
		let defaults = version != Version::Draft18;
		encode_params!(w, version,
			0x03 => self.authorization_token.clone(),
			0x10 => (!self.forward).then_some(false),
			0x22 => self.group_order.filter(|_| defaults),
			0x35 => include_properties,
		);
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let prefix = decode_namespace(r)?;
		// From draft-20 any parameter a SUBSCRIBE takes is legal here (section 3.6.2 of draft-22);
		// draft-18 allows only AUTHORIZATION_TOKEN and FORWARD. Those that only make sense
		// on one subscription, such as RENDEZVOUS_TIMEOUT or a delivery timeout, are read
		// and dropped.
		let defaults = version != Version::Draft18;
		decode_params!(r, version,
			0x02 => _object_delivery_timeout: Option<u64> where defaults,
			0x03 => authorization_token: Vec<RequestToken>,
			0x04 => _rendezvous_timeout: Option<u64> where defaults,
			0x06 => _subgroup_delivery_timeout: Option<u64> where defaults,
			0x10 => forward: Option<bool>,
			0x20 => _subscriber_priority: Option<u8> where defaults,
			0x21 => _filter: Option<Filter> where defaults,
			0x22 => group_order: Option<GroupOrder> where defaults,
			// Each PUBLISH starts paused, where a fill opens nothing, even once resumed
			// (draft-22 section 3.4.1), so FILL_PARAMETERS is read and dropped.
			0x23 => _fill: Option<super::Fill> where Filter::is_draft20(version),
			0x25 => _subgroup_filter: Vec<Opaque> where has_range_filters(version),
			0x26 => _object_id_filter: Vec<Opaque> where has_range_filters(version),
			0x27 => _priority_filter: Vec<Opaque> where has_range_filters(version),
			0x28 => _object_property_filter: Vec<Opaque> where has_range_filters(version),
			0x29 => _track_property_filter: Vec<Opaque> where has_range_filters(version),
			0x32 => _new_group: Option<u64> where defaults,
			0x35 => include_properties: Option<bool> where Filter::is_draft20(version),
		);
		// Last, so a malformed field closes the session before a token refusal.
		let authorization_token = super::token::single(authorization_token)?;

		Ok(Self {
			request_id,
			prefix,
			forward: forward.unwrap_or(true),
			group_order,
			properties_wanted: include_properties.unwrap_or(true),
			authorization_token,
		})
	}
}

/// PUBLISH_SKIPPED (0x0F): a track under a SUBSCRIBE_TRACKS prefix the publisher will
/// not PUBLISH, sent on that request's stream. The subscriber may SUBSCRIBE to it instead.
///
/// Draft-18 names it PUBLISH_BLOCKED and send it only when the publisher has run
/// out of streams for PUBLISH; draft-22 renamed it and allows it for any reason.
#[derive(Clone, Debug)]
pub struct PublishSkipped<'a> {
	/// The track's namespace below the request's prefix.
	pub suffix: Path,
	pub track_name: Cow<'a, str>,
}

impl<'a> Message<'a> for PublishSkipped<'a> {
	const ID: u64 = 0x0F;

	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		encode_namespace(w, &self.suffix)?;
		w.string(&self.track_name)?;
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let suffix = decode_namespace(r)?;
		let track_name = Cow::Borrowed(r.str()?);
		Ok(Self { suffix, track_name })
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn encode<'m, M: Message<'m>>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf
	}

	fn decode<'m, M: Message<'m>>(bytes: &'m [u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes;
		crate::coding::decode_from(&mut buf, version, |r, v| M::decode_msg(r, v))
	}

	fn paused() -> SubscribeTracks {
		SubscribeTracks {
			request_id: RequestId(4),
			prefix: Path::from_static("room"),
			forward: false,
			group_order: None,
			properties_wanted: true,
			authorization_token: None,
		}
	}

	#[test]
	fn a_paused_listing_round_trips() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let decoded: SubscribeTracks = decode(&encode(&paused(), version), version).unwrap();
			assert_eq!(decoded.request_id, RequestId(4), "{version}");
			assert_eq!(decoded.prefix.as_str(), "room", "{version}");
			assert!(!decoded.forward, "{version}");
		}
	}

	/// FORWARD defaults to 1, so a listing that omits it asks for every track's objects.
	#[test]
	fn an_omitted_forward_means_one() {
		// Request ID 4, prefix "room", no parameters.
		let bytes = [0x04, 0x01, 0x04, b'r', b'o', b'o', b'm', 0x00];
		let decoded: SubscribeTracks = decode(&bytes, Version::Draft22).unwrap();
		assert!(decoded.forward);
	}

	/// TRACK_PROPERTY_FILTER (draft-22) is legal, so it is read.
	#[test]
	fn a_track_property_filter_is_read() {
		// Request ID 4, prefix "room", one parameter: 0x29 with a two-byte opaque value.
		let bytes = [0x04, 0x01, 0x04, b'r', b'o', b'o', b'm', 0x01, 0x29, 0x02, 0xaa, 0xbb];
		assert!(decode::<SubscribeTracks>(&bytes, Version::Draft22).is_ok());
		// Draft-18 has no such parameter, and an unknown even type is malformed there.
		assert!(decode::<SubscribeTracks>(&bytes, Version::Draft18).is_err());
	}

	/// Draft-18 carries no subscription parameters on SUBSCRIBE_TRACKS: one is not sent,
	/// and one received is malformed.
	#[test]
	fn draft_18_carries_no_subscription_parameters() {
		let listing = SubscribeTracks {
			group_order: Some(GroupOrder::Ascending),
			..paused()
		};
		assert_eq!(encode(&listing, Version::Draft18), encode(&paused(), Version::Draft18));
		// Request ID 4, prefix "room", one parameter: SUBSCRIBER_PRIORITY 7.
		let bytes = [0x04, 0x01, 0x04, b'r', b'o', b'o', b'm', 0x01, 0x20, 0x07];
		assert!(decode::<SubscribeTracks>(&bytes, Version::Draft18).is_err());
	}

	/// From draft-20 GROUP_ORDER is sent, and any subscription parameter received is read.
	#[test]
	fn later_drafts_send_their_group_order() {
		let ordered = SubscribeTracks {
			group_order: Some(GroupOrder::Ascending),
			..paused()
		};
		for version in [Version::Draft20, Version::Draft22] {
			assert_ne!(encode(&ordered, version), encode(&paused(), version), "{version}");
			let decoded: SubscribeTracks = decode(&encode(&ordered, version), version).unwrap();
			assert_eq!(decoded.group_order, Some(GroupOrder::Ascending), "{version}");
			// Request ID 4, prefix "room", one parameter: SUBSCRIBER_PRIORITY 7.
			let bytes = [0x04, 0x01, 0x04, b'r', b'o', b'o', b'm', 0x01, 0x20, 0x07];
			assert!(decode::<SubscribeTracks>(&bytes, version).is_ok(), "{version}");
		}
	}

	#[test]
	fn publish_skipped_round_trips() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let msg = PublishSkipped {
				suffix: Path::from_static("cam"),
				track_name: "video".into(),
			};
			let wire = encode(&msg, version);
			let decoded: PublishSkipped = decode(&wire, version).unwrap();
			assert_eq!(decoded.suffix.as_str(), "cam", "{version}");
			assert_eq!(decoded.track_name, "video", "{version}");
		}
	}
}
