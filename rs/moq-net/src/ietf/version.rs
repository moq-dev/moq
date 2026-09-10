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
		Fetch, FetchType, Fill, Filter, GroupFlags, GroupHeader, GroupOrder, Location, Message, Properties, Publish,
		RequestId, Subscribe, SubscribeOk,
	};
	use crate::{Path, Timescale};

	fn message<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut buf, version).expect("encode");
		buf
	}

	fn field<E: Encode<Version>>(value: &E, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		value.encode(&mut buf, version).expect("encode");
		buf
	}

	/// Draft-21 is editorial: it restructures the document and drops the Connection URL,
	/// Stream Cancellation, and Examples sections, leaving every codepoint and field
	/// layout as draft-20 defined them. So it has to encode byte for byte the same, and
	/// this pins that rather than trusting each version branch to fall forward.
	#[test]
	fn draft21_matches_draft20_on_the_wire() {
		let properties = Properties {
			timescale: Some(Timescale::new(90_000).unwrap()),
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
			fetch_type: FetchType::Standalone {
				namespace: Path::new("broadcast"),
				track: "video".into(),
				start: Location { group: 1, object: 0 },
				end: Location { group: 2, object: 0 },
			},
		};

		let group = GroupHeader {
			track_alias: 4,
			group_id: 11,
			sub_group_id: 0,
			publisher_priority: 128,
			flags: GroupFlags::default(),
		};

		assert_eq!(
			message(&subscribe, Version::Draft20),
			message(&subscribe, Version::Draft21),
			"SUBSCRIBE"
		);
		assert_eq!(
			message(&subscribe_ok, Version::Draft20),
			message(&subscribe_ok, Version::Draft21),
			"SUBSCRIBE_OK"
		);
		assert_eq!(
			message(&publish, Version::Draft20),
			message(&publish, Version::Draft21),
			"PUBLISH"
		);
		assert_eq!(
			message(&fetch, Version::Draft20),
			message(&fetch, Version::Draft21),
			"FETCH"
		);
		assert_eq!(
			field(&group, Version::Draft20),
			field(&group, Version::Draft21),
			"SUBGROUP_HEADER"
		);
	}
}
