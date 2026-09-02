use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode, EncodeError},
	ietf::{
		GroupOrder, Location, Parameters, RequestId,
		namespace::{decode_namespace, encode_namespace},
	},
};

use super::Message;

use super::Version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchType<'a> {
	//
	Standalone {
		namespace: Path<'a>,
		track: Cow<'a, str>,
		start: Location,
		end: Location,
	},
	RelativeJoining {
		subscriber_request_id: RequestId,
		group_offset: u64,
	},
	AbsoluteJoining {
		subscriber_request_id: RequestId,
		group_id: u64,
	},
}

impl Encode<Version> for FetchType<'_> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match self {
			FetchType::Standalone {
				namespace,
				track,
				start,
				end,
			} => {
				1u8.encode(w, version)?;
				encode_namespace(w, namespace, version)?;
				track.encode(w, version)?;
				start.encode(w, version)?;
				end.encode(w, version)?;
			}
			FetchType::RelativeJoining {
				subscriber_request_id,
				group_offset,
			} => {
				2u8.encode(w, version)?;
				subscriber_request_id.encode(w, version)?;
				group_offset.encode(w, version)?;
			}
			FetchType::AbsoluteJoining {
				subscriber_request_id,
				group_id,
			} => {
				3u8.encode(w, version)?;
				subscriber_request_id.encode(w, version)?;
				group_id.encode(w, version)?;
			}
		}
		Ok(())
	}
}

impl Decode<Version> for FetchType<'_> {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let fetch_type = u64::decode(buf, version)?;
		Ok(match fetch_type {
			0x1 => {
				let namespace = decode_namespace(buf, version)?;
				let track = Cow::<str>::decode(buf, version)?;
				let start = Location::decode(buf, version)?;
				let end = Location::decode(buf, version)?;
				FetchType::Standalone {
					namespace,
					track,
					start,
					end,
				}
			}
			0x2 => {
				let subscriber_request_id = RequestId::decode(buf, version)?;
				let group_offset = u64::decode(buf, version)?;
				FetchType::RelativeJoining {
					subscriber_request_id,
					group_offset,
				}
			}
			0x3 => {
				let subscriber_request_id = RequestId::decode(buf, version)?;
				let group_id = u64::decode(buf, version)?;
				FetchType::AbsoluteJoining {
					subscriber_request_id,
					group_id,
				}
			}
			_ => return Err(DecodeError::InvalidValue),
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fetch<'a> {
	pub request_id: RequestId,
	pub subscriber_priority: u8,
	pub group_order: GroupOrder,
	pub fetch_type: FetchType<'a>,
}

impl Message for Fetch<'_> {
	const ID: u64 = 0x16;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			0u64.encode(w, version)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}

		match version {
			Version::Draft14 => {
				self.subscriber_priority.encode(w, version)?;
				self.group_order.encode(w, version)?;
				self.fetch_type.encode(w, version)?;
				0u8.encode(w, version)?; // no parameters
			}
			_ => {
				self.fetch_type.encode(w, version)?;
				encode_params!(w, version,
					0x20 => self.subscriber_priority,
					0x22 => self.group_order,
				);
			}
		}
		Ok(())
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = u64::decode(buf, version)?;
		}

		match version {
			Version::Draft14 => {
				let subscriber_priority = u8::decode(buf, version)?;
				let group_order = GroupOrder::decode(buf, version)?;
				let fetch_type = FetchType::decode(buf, version)?;
				let _params = Parameters::decode(buf, version)?;
				Ok(Self {
					request_id,
					subscriber_priority,
					group_order,
					fetch_type,
				})
			}
			_ => {
				let fetch_type = FetchType::decode(buf, version)?;
				decode_params!(buf, version,
					0x20 => subscriber_priority: Option<u8>,
					0x22 => group_order: Option<GroupOrder>,
				);

				let subscriber_priority = subscriber_priority.unwrap_or(128);
				let group_order = group_order.unwrap_or(GroupOrder::Descending);

				Ok(Self {
					request_id,
					subscriber_priority,
					group_order,
					fetch_type,
				})
			}
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOk {
	pub request_id: Option<RequestId>,
	pub group_order: GroupOrder,
	pub end_of_track: bool,
	pub end_location: Location,
}
impl Message for FetchOk {
	const ID: u64 = 0x18;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			self.request_id
				.expect("request_id required for draft14-16")
				.encode(w, version)?;
		} else {
			assert!(self.request_id.is_none(), "request_id must be None for draft17+");
		}

		match version {
			Version::Draft14 => {
				self.group_order.encode(w, version)?;
				self.end_of_track.encode(w, version)?;
				self.end_location.encode(w, version)?;
				0u8.encode(w, version)?; // no parameters
			}
			_ => {
				// GROUP_ORDER is not a legal FETCH_OK parameter in any draft after 14; the order
				// of the response is whatever the FETCH asked for.
				self.end_of_track.encode(w, version)?;
				self.end_location.encode(w, version)?;
				encode_params!(w, version,);
			}
		}
		Ok(())
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			Some(RequestId::decode(buf, version)?)
		} else {
			None
		};

		match version {
			Version::Draft14 => {
				let group_order = GroupOrder::decode(buf, version)?;
				let end_of_track = bool::decode(buf, version)?;
				let end_location = Location::decode(buf, version)?;
				let _params = Parameters::decode(buf, version)?;
				Ok(Self {
					request_id,
					group_order,
					end_of_track,
					end_location,
				})
			}
			_ => {
				let end_of_track = bool::decode(buf, version)?;
				let end_location = Location::decode(buf, version)?;
				// GROUP_ORDER isn't legal here, but keep accepting it so a peer that still sends
				// it doesn't have its session torn down over a hint.
				decode_params!(buf, version,
					0x22 => group_order: Option<GroupOrder>,
				);
				// FETCH_OK may declare a timescale; we don't surface it yet, and a fetched
				// object without an interpretable timestamp is stamped on arrival.
				let _ = super::Properties::decode(buf, version)?;

				let group_order = group_order.unwrap_or(GroupOrder::Descending);

				Ok(Self {
					request_id,
					group_order,
					end_of_track,
					end_location,
				})
			}
		}
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchError<'a> {
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for FetchError<'_> {
	const ID: u64 = 0x19;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		self.error_code.encode(w, version)?;
		self.reason_phrase.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		let error_code = u64::decode(buf, version)?;
		let reason_phrase = Cow::<str>::decode(buf, version)?;
		Ok(Self {
			request_id,
			error_code,
			reason_phrase,
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchCancel {
	pub request_id: RequestId,
}
impl Message for FetchCancel {
	const ID: u64 = 0x17;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		Ok(Self { request_id })
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchHeader {
	pub request_id: RequestId,
}

impl FetchHeader {
	pub const TYPE: u64 = 0x5;
}

impl Encode<Version> for FetchHeader {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}
}

impl Decode<Version> for FetchHeader {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		Ok(Self { request_id })
	}
}

/// The bits of an Object's Serialization Flags (draft-20 section 11.4.4.1).
mod flag {
	/// The two low bits, which spell the Subgroup ID rather than a presence bit.
	pub const SUBGROUP: u64 = 0x03;
	pub const OBJECT_ID: u64 = 0x04;
	pub const GROUP_ID: u64 = 0x08;
	pub const PRIORITY: u64 = 0x10;
	pub const PROPERTIES: u64 = 0x20;
	/// The object was published as a datagram, so it has no Subgroup ID at all and the
	/// two low bits mean nothing.
	pub const DATAGRAM: u64 = 0x40;
}

/// How an Object on a fetch stream names its Subgroup ID: the two low bits of the
/// Serialization Flags.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum FetchSubgroup {
	/// Subgroup zero.
	#[default]
	Zero,
	/// The prior Object's Subgroup ID.
	Prior,
	/// One past the prior Object's Subgroup ID.
	PriorPlusOne,
	/// Spelled out on the wire.
	Explicit(u64),
	/// A datagram Object, which has no Subgroup ID.
	Datagram,
}

/// One Object on a fetch stream, from its Serialization Flags through its Properties
/// (draft-20 section 11.4.4).
///
/// The Object Payload Length and payload follow on the wire; they are streamed by the
/// caller rather than buffered here, which is what keeps a large frame off the heap twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchObject {
	/// An Object. A field is on the wire only when its flag says so, and an absent one
	/// inherits from the prior Object on the stream.
	Object {
		/// How the Subgroup ID is spelled.
		subgroup: FetchSubgroup,

		/// The Group ID Delta. On the first Object this is the absolute Group ID; on any
		/// later one it names a *different* group (the prior one plus or minus the delta
		/// plus one, by group order).
		group: Option<u64>,

		/// The Object ID Delta. Absolute when `group` is present, otherwise added to the
		/// prior Object ID. Absent means the prior ID plus one.
		object: Option<u64>,

		/// The Publisher Priority, absent when it repeats the prior Object's.
		priority: Option<u8>,

		/// The Object Properties block, which carries the Timestamp.
		properties: Option<Vec<u8>>,
	},

	/// An End of Range marker: every Location between the prior Object and this one,
	/// inclusive, does not exist (`0x8C`), is unknown (`0x10C`), or timed out (`0x20C`).
	///
	/// The Group and Object IDs are the same delta fields an [`Self::Object`] carries.
	EndOfRange {
		/// The raw Serialization Flags, which is which of the three it is.
		reason: u64,
		/// The Group ID Delta.
		group: u64,
		/// The Object ID Delta.
		object: u64,
	},
}

impl FetchObject {
	/// The Serialization Flags values that mark an End of Range instead of an Object.
	const END_OF_RANGE: &'static [u64] = &[0x8C, 0x10C, 0x20C];
}

impl Encode<Version> for FetchObject {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match self {
			Self::EndOfRange { reason, group, object } => {
				if !Self::END_OF_RANGE.contains(reason) {
					return Err(EncodeError::InvalidState);
				}
				reason.encode(w, version)?;
				group.encode(w, version)?;
				object.encode(w, version)?;
			}
			Self::Object {
				subgroup,
				group,
				object,
				priority,
				properties,
			} => {
				let mut flags = match subgroup {
					FetchSubgroup::Zero => 0,
					FetchSubgroup::Prior => 1,
					FetchSubgroup::PriorPlusOne => 2,
					FetchSubgroup::Explicit(_) => 3,
					FetchSubgroup::Datagram => flag::DATAGRAM,
				};
				if group.is_some() {
					flags |= flag::GROUP_ID;
				}
				if object.is_some() {
					flags |= flag::OBJECT_ID;
				}
				if priority.is_some() {
					flags |= flag::PRIORITY;
				}
				if properties.is_some() {
					flags |= flag::PROPERTIES;
				}
				flags.encode(w, version)?;

				if let Some(group) = group {
					group.encode(w, version)?;
				}
				if let FetchSubgroup::Explicit(subgroup) = subgroup {
					subgroup.encode(w, version)?;
				}
				if let Some(object) = object {
					object.encode(w, version)?;
				}
				if let Some(priority) = priority {
					priority.encode(w, version)?;
				}
				if let Some(properties) = properties {
					properties.encode(w, version)?;
				}
			}
		}
		Ok(())
	}
}

impl Decode<Version> for FetchObject {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let flags = u64::decode(buf, version)?;

		// Anything at or above 128 is a named value rather than a set of flags, and only
		// the three End of Range markers are defined.
		if flags >= 0x80 {
			if !Self::END_OF_RANGE.contains(&flags) {
				return Err(DecodeError::InvalidValue);
			}
			return Ok(Self::EndOfRange {
				reason: flags,
				group: u64::decode(buf, version)?,
				object: u64::decode(buf, version)?,
			});
		}

		// Wire order: Group ID Delta, Subgroup ID, Object ID Delta, Priority, Properties.
		let group = match flags & flag::GROUP_ID != 0 {
			true => Some(u64::decode(buf, version)?),
			false => None,
		};

		let subgroup = match flags & flag::DATAGRAM != 0 {
			true => FetchSubgroup::Datagram,
			false => match flags & flag::SUBGROUP {
				0 => FetchSubgroup::Zero,
				1 => FetchSubgroup::Prior,
				2 => FetchSubgroup::PriorPlusOne,
				_ => FetchSubgroup::Explicit(u64::decode(buf, version)?),
			},
		};

		let object = match flags & flag::OBJECT_ID != 0 {
			true => Some(u64::decode(buf, version)?),
			false => None,
		};

		let priority = match flags & flag::PRIORITY != 0 {
			true => Some(u8::decode(buf, version)?),
			false => None,
		};

		let properties = match flags & flag::PROPERTIES != 0 {
			true => Some(Vec::<u8>::decode(buf, version)?),
			false => None,
		};

		Ok(Self::Object {
			subgroup,
			group,
			object,
			priority,
			properties,
		})
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
	fn test_fetch_v14_round_trip() {
		let msg = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Standalone {
				namespace: Path::new("test"),
				track: "video".into(),
				start: Location { group: 0, object: 0 },
				end: Location { group: 10, object: 5 },
			},
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: Fetch = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_fetch_v15_round_trip() {
		let msg = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Standalone {
				namespace: Path::new("test"),
				track: "video".into(),
				start: Location { group: 0, object: 0 },
				end: Location { group: 10, object: 5 },
			},
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: Fetch = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_fetch_ok_v14_round_trip() {
		let msg = FetchOk {
			request_id: Some(RequestId(2)),
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: FetchOk = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(2)));
		assert!(!decoded.end_of_track);
		assert_eq!(decoded.end_location, Location { group: 5, object: 3 });
	}

	#[test]
	fn test_fetch_v16_round_trip() {
		let msg = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Standalone {
				namespace: Path::new("test"),
				track: "video".into(),
				start: Location { group: 0, object: 0 },
				end: Location { group: 10, object: 5 },
			},
		};

		let encoded = encode_message(&msg, Version::Draft16);
		let decoded: Fetch = decode_message(&encoded, Version::Draft16).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_fetch_v17_round_trip() {
		let msg = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Standalone {
				namespace: Path::new("test"),
				track: "video".into(),
				start: Location { group: 0, object: 0 },
				end: Location { group: 10, object: 5 },
			},
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: Fetch = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_fetch_ok_v15_round_trip() {
		let msg = FetchOk {
			request_id: Some(RequestId(2)),
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: FetchOk = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(2)));
		assert!(!decoded.end_of_track);
		assert_eq!(decoded.end_location, Location { group: 5, object: 3 });
	}

	#[test]
	fn test_fetch_ok_v16_round_trip() {
		let msg = FetchOk {
			request_id: Some(RequestId(2)),
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		let encoded = encode_message(&msg, Version::Draft16);
		let decoded: FetchOk = decode_message(&encoded, Version::Draft16).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(2)));
		assert!(!decoded.end_of_track);
		assert_eq!(decoded.end_location, Location { group: 5, object: 3 });
	}

	#[test]
	fn test_fetch_ok_v17_round_trip() {
		let msg = FetchOk {
			request_id: None,
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: FetchOk = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, None);
		assert!(!decoded.end_of_track);
		assert_eq!(decoded.end_location, Location { group: 5, object: 3 });
	}

	#[test]
	fn test_fetch_v18_round_trip() {
		let msg = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Standalone {
				namespace: Path::new("test"),
				track: "video".into(),
				start: Location { group: 0, object: 0 },
				end: Location { group: 10, object: 5 },
			},
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: Fetch = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(1));
		assert_eq!(decoded.subscriber_priority, 128);
	}

	#[test]
	fn test_fetch_ok_v18_round_trip() {
		let msg = FetchOk {
			request_id: None,
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: FetchOk = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, None);
		assert!(!decoded.end_of_track);
		assert_eq!(decoded.end_location, Location { group: 5, object: 3 });
	}

	/// GROUP_ORDER (0x22) has never been a legal FETCH_OK parameter outside draft-14, where
	/// it was a plain field. A draft-15+ peer closes the session with PROTOCOL_VIOLATION when
	/// it sees one, so the response carries no parameters at all.
	#[test]
	fn test_fetch_ok_v18_omits_group_order() {
		let msg = FetchOk {
			request_id: None,
			group_order: GroupOrder::Descending,
			end_of_track: false,
			end_location: Location { group: 5, object: 3 },
		};

		#[rustfmt::skip]
		let expected = vec![
			0, // end of track
			5, // end group
			3, // end object
			0, // zero message parameters
		];
		assert_eq!(encode_message(&msg, Version::Draft18), expected);
	}
}

/// The Object serialization on a fetch stream (draft-20 section 11.4.4), which a fill's
/// head arrives on.
#[cfg(test)]
mod object_tests {
	use super::*;
	use bytes::{Buf as _, BytesMut};

	const VERSION: Version = Version::Draft20;

	fn round_trip(object: &FetchObject) -> (Vec<u8>, FetchObject) {
		let mut buf = BytesMut::new();
		object.encode(&mut buf, VERSION).expect("encode");

		let mut bytes = bytes::Bytes::from(buf.to_vec());
		let decoded = FetchObject::decode(&mut bytes, VERSION).expect("decode");
		assert!(!bytes.has_remaining(), "the object header is fully consumed");

		(buf.to_vec(), decoded)
	}

	/// The first Object carries absolute IDs and a priority, because "same as the prior
	/// Object" has no prior to refer to. Byte-pinned: the flags declare exactly the fields
	/// that follow, in wire order.
	#[test]
	fn the_first_object_spells_everything_out() {
		let object = FetchObject::Object {
			subgroup: FetchSubgroup::Zero,
			group: Some(4),
			object: Some(0),
			priority: Some(0),
			properties: Some(vec![0x02, 0x40]),
		};

		let (encoded, decoded) = round_trip(&object);
		assert_eq!(decoded, object);

		#[rustfmt::skip]
		let expected = vec![
			0x3C, // GROUP_ID | OBJECT_ID | PRIORITY | PROPERTIES, subgroup zero
			0x04, // group 4
			0x00, // object 0
			0x00, // priority
			0x02, 0x02, 0x40, // 2 bytes of properties
		];
		assert_eq!(encoded, expected);
	}

	/// Every later Object inherits the group, subgroup and priority, and its ID is the prior
	/// one plus one, so only the properties go on the wire.
	#[test]
	fn a_later_object_inherits() {
		let object = FetchObject::Object {
			subgroup: FetchSubgroup::Zero,
			group: None,
			object: None,
			priority: None,
			properties: Some(vec![]),
		};

		let (encoded, decoded) = round_trip(&object);
		assert_eq!(decoded, object);
		assert_eq!(encoded, vec![0x20, 0x00]);
	}

	/// The two low bits spell the Subgroup ID rather than a presence bit, and the datagram
	/// flag says there is none at all.
	#[test]
	fn the_subgroup_is_spelled_by_the_low_bits() {
		for subgroup in [
			FetchSubgroup::Zero,
			FetchSubgroup::Prior,
			FetchSubgroup::PriorPlusOne,
			FetchSubgroup::Explicit(9),
			FetchSubgroup::Datagram,
		] {
			let object = FetchObject::Object {
				subgroup,
				group: None,
				object: None,
				priority: None,
				properties: None,
			};
			assert_eq!(round_trip(&object).1, object, "{subgroup:?}");
		}
	}

	/// An End of Range is a named value rather than a set of flags, and its two IDs are
	/// always present.
	#[test]
	fn an_end_of_range_carries_its_location() {
		for reason in [0x8C, 0x10C, 0x20C] {
			let object = FetchObject::EndOfRange {
				reason,
				group: 3,
				object: 7,
			};
			assert_eq!(round_trip(&object).1, object, "{reason:#x}");
		}
	}

	/// Every other value at or above 128 is undefined, and reading one as flags would
	/// desync the rest of the stream.
	#[test]
	fn an_undefined_value_is_refused() {
		// 0x8D, one past End of Non-Existent Range, in the draft-17+ leading-ones form.
		let mut bytes = bytes::Bytes::from_static(&[0x80, 0x8D, 0x00, 0x00]);
		assert!(FetchObject::decode(&mut bytes, VERSION).is_err());
	}
}
