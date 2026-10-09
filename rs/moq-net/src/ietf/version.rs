use std::fmt;

/// An IETF protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Version {
	Draft14,
	Draft15,
	Draft16,
	Draft17,
	Draft18,
	Draft19,
	Draft20,
	Draft21,
	Draft22,
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Draft14 => write!(f, "moq-transport-14"),
			Self::Draft15 => write!(f, "moq-transport-15"),
			Self::Draft16 => write!(f, "moq-transport-16"),
			Self::Draft17 => write!(f, "moq-transport-17"),
			Self::Draft18 => write!(f, "moq-transport-18"),
			Self::Draft19 => write!(f, "moq-transport-19"),
			Self::Draft20 => write!(f, "moq-transport-20"),
			Self::Draft21 => write!(f, "moq-transport-21"),
			Self::Draft22 => write!(f, "moq-transport-22"),
		}
	}
}

impl From<Version> for crate::Version {
	fn from(v: Version) -> Self {
		crate::Version::Ietf(v)
	}
}

impl TryFrom<crate::Version> for Version {
	type Error = ();

	fn try_from(v: crate::Version) -> Result<Self, Self::Error> {
		match v {
			crate::Version::Ietf(v) => Ok(v),
			crate::Version::Lite(_) => Err(()),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::Encode;
	use crate::ietf::{
		EndLocation, Fetch, FetchType, Fill, Filter, GroupFlags, GroupHeader, GroupOrder, Location, Message,
		Properties, Publish, RequestId, Subscribe, SubscribeOk,
	};
	use crate::{Path, Timescale};

	fn message<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
			.expect("encode");
		buf
	}

	fn field<E: Encode<Version>>(value: &E, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		value
			.encode(&mut crate::coding::Encoder::new(&mut buf, version.into()), version)
			.expect("encode");
		buf
	}

	/// Draft-21 is editorial: it restructures the document and drops the Connection URL,
	/// Stream Cancellation, and Examples sections, leaving every codepoint and field layout
	/// as draft-20 defined them. So it has to encode byte for byte the same, and this pins
	/// that rather than trusting each version branch to fall forward.
	#[test]
	fn draft21_matches_draft20_on_the_wire() {
		let properties = Properties {
			max_cache_duration: None,
			timescale: Some(Timescale::new(90_000).unwrap()),
			priority: Some(64),
			group_order: Some(GroupOrder::Descending),
		};

		let subscribe = Subscribe {
			request_id: RequestId(1),
			track_namespace: Path::new("broadcast"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::Relative(3),
			fill: Some(Fill {
				filter: Some(Filter::Relative(1)),
				range_filters: false,
			}),
			properties_wanted: false,
			forward: true,
			range_filters: false,
			authorization_token: None,
		};

		let subscribe_ok = SubscribeOk {
			// Draft-17 and newer carry the request on the stream, not in the message.
			request_id: None,
			track_alias: 4,
			largest: Some(Location { group: 5, object: 2 }),
			properties,
		};

		let publish = Publish {
			request_id: RequestId(2),
			track_namespace: Path::new("broadcast"),
			track_name: "audio".into(),
			track_alias: 7,
			largest_location: Some(Location { group: 9, object: 0 }),
			forward: true,
			properties,
		};

		let fetch = Fetch {
			request_id: RequestId(3),
			subscriber_priority: 64,
			group_order: GroupOrder::Ascending,
			fetch_type: FetchType::Filtered {
				namespace: Path::new("broadcast"),
				track: "video".into(),
				filter: Filter::Absolute {
					start: Location { group: 1, object: 0 },
					end: Some(EndLocation {
						group: 2,
						object: Some(0),
					}),
				},
			},
			range_filters: false,
			fill_timeout: false,
			properties_wanted: false,
		};

		let group = GroupHeader {
			track_alias: 4,
			group_id: 11,
			sub_group_id: 0,
			publisher_priority: 128,
			flags: GroupFlags::default(),
		};

		let (old, new) = (Version::Draft20, Version::Draft21);
		assert_eq!(message(&subscribe, old), message(&subscribe, new), "SUBSCRIBE");
		assert_eq!(message(&subscribe_ok, old), message(&subscribe_ok, new), "SUBSCRIBE_OK");
		assert_eq!(message(&publish, old), message(&publish, new), "PUBLISH");
		assert_eq!(message(&fetch, old), message(&fetch, new), "FETCH");
		assert_eq!(field(&group, old), field(&group, new), "SUBGROUP_HEADER");
	}

	/// Draft-22's only wire change is LOCATION_FILTER, which drops its Length for a Location
	/// Filter Type. So a SUBSCRIBE differs from draft-20 in those bytes alone.
	#[test]
	fn draft22_changes_only_the_location_filter() {
		let subscribe = Subscribe {
			request_id: RequestId(1),
			track_namespace: Path::new("broadcast"),
			track_name: "video".into(),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			filter: Filter::NextObject,
			fill: None,
			properties_wanted: true,
			forward: true,
			range_filters: false,
			authorization_token: None,
		};

		// Draft-20 spells Next Object as a Length of 2 and two zero fields; draft-22 as type 0x05.
		let old = message(&subscribe, Version::Draft20);
		let at = old
			.windows(4)
			.position(|w| w == [0x01, 0x02, 0x00, 0x00])
			.expect("draft-20 LOCATION_FILTER");
		let mut expected = old[..at].to_vec();
		expected.extend_from_slice(&[0x01, 0x05]);
		expected.extend_from_slice(&old[at + 4..]);

		assert_eq!(message(&subscribe, Version::Draft22), expected);
	}
}
