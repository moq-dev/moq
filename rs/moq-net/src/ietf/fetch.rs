use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder},
	ietf::{
		Filter, GroupOrder, Location, Opaque, Parameters, RequestId,
		namespace::{decode_namespace, encode_namespace},
		subscribe::has_range_filters,
	},
};

use super::Message;

use super::Version;

/// What a FETCH asks for.
///
/// Through draft-19 a Fetch Type tag picks one of the first three. Draft-20 dropped the
/// tag and the joining forms, leaving [`Self::Filtered`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FetchType<'a> {
	/// An inclusive range of a track, through draft-19.
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
	/// A track, with the range as a LOCATION_FILTER, from draft-20 on. Unfiltered is
	/// everything from `{0, 0}` up to Largest Object.
	Filtered {
		namespace: Path<'a>,
		track: Cow<'a, str>,
		filter: Filter,
	},
}

impl Encode<Version> for FetchType<'_> {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match self {
			FetchType::Standalone {
				namespace,
				track,
				start,
				end,
			} => {
				w.u8(1);
				encode_namespace(w, namespace)?;
				w.string(track)?;
				start.encode(w, version)?;
				end.encode(w, version)?;
			}
			FetchType::RelativeJoining {
				subscriber_request_id,
				group_offset,
			} => {
				w.u8(2);
				subscriber_request_id.encode(w, version)?;
				w.varint(*group_offset)?;
			}
			FetchType::AbsoluteJoining {
				subscriber_request_id,
				group_id,
			} => {
				w.u8(3);
				subscriber_request_id.encode(w, version)?;
				w.varint(*group_id)?;
			}
			// Draft-20 has no Fetch Type tag to write.
			FetchType::Filtered { .. } => return Err(EncodeError::Version),
		}
		Ok(())
	}
}

impl Decode<Version> for FetchType<'_> {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let fetch_type = buf.varint()?;
		Ok(match fetch_type {
			0x1 => {
				let namespace = decode_namespace(buf)?;
				let track = Cow::Owned(buf.string()?);
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
				let group_offset = buf.varint()?;
				FetchType::RelativeJoining {
					subscriber_request_id,
					group_offset,
				}
			}
			0x3 => {
				let subscriber_request_id = RequestId::decode(buf, version)?;
				let group_id = buf.varint()?;
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
	/// Whether the request carried a Range Filter (0x25-0x28). We advertise no
	/// MAX_FILTER_RANGES, so the request is refused rather than served unfiltered.
	/// Never encoded; we send no range filters.
	pub range_filters: bool,
	/// Whether the request carried FILL_TIMEOUT (0x0A), a budget for waiting on upstream
	/// that ends in Timed-Out gaps we cannot write, so the request is refused rather than
	/// left to wait without it. Never encoded; we send no fill timeout.
	pub fill_timeout: bool,
}

impl Message for Fetch<'_> {
	const ID: u64 = 0x16;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		// GROUP_ORDER allows only Ascending or Descending, so no preference is an absent
		// parameter rather than a 0 the peer must treat as a protocol violation.
		let group_order = (self.group_order != GroupOrder::Any).then_some(self.group_order);

		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			w.varint(0)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}

		match version {
			Version::Draft14 => {
				w.u8(self.subscriber_priority);
				self.group_order.encode(w, version)?;
				self.fetch_type.encode(w, version)?;
				w.u8(0); // no parameters
			}
			Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18 | Version::Draft19 => {
				self.fetch_type.encode(w, version)?;
				encode_params!(w, version,
					0x20 => self.subscriber_priority,
					0x22 => group_order,
				);
			}
			_ => {
				// The joining forms and the Fetch Type tag are gone in draft-20.
				let FetchType::Filtered {
					namespace,
					track,
					filter,
				} = &self.fetch_type
				else {
					return Err(EncodeError::Version);
				};
				encode_namespace(w, namespace)?;
				w.string(track)?;
				encode_params!(w, version,
					0x20 => self.subscriber_priority,
					0x21 => *filter,
					0x22 => group_order,
				);
			}
		}
		Ok(())
	}

	fn decode_msg(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = buf.varint()?;
		}

		// The token is ignored: the session's grant is what authorizes the request, and
		// INCLUDE_PROPERTIES only shapes a FETCH_OK we don't send on draft-20.
		let (fetch_type, subscriber_priority, group_order, range_filters, fill_timeout) = match version {
			Version::Draft14 => {
				let subscriber_priority = buf.u8()?;
				let group_order = GroupOrder::decode(buf, version)?;
				let fetch_type = FetchType::decode(buf, version)?;
				Parameters::skip(buf)?;
				(fetch_type, Some(subscriber_priority), Some(group_order), false, false)
			}
			Version::Draft15 | Version::Draft16 | Version::Draft17 | Version::Draft18 | Version::Draft19 => {
				let fetch_type = FetchType::decode(buf, version)?;
				decode_params!(buf, version,
					0x03 => _authorization_token: Vec<Opaque>,
					0x0A => fill_timeout: Option<u64> where !matches!(version, Version::Draft15 | Version::Draft16 | Version::Draft17),
					0x20 => subscriber_priority: Option<u8>,
					0x22 => group_order: Option<GroupOrder>,
					0x25 => subgroup_filter: Vec<Opaque> where has_range_filters(version),
					0x26 => object_id_filter: Vec<Opaque> where has_range_filters(version),
					0x27 => priority_filter: Vec<Opaque> where has_range_filters(version),
					0x28 => object_property_filter: Vec<Opaque> where has_range_filters(version),
				);
				let range_filters = [
					subgroup_filter,
					object_id_filter,
					priority_filter,
					object_property_filter,
				]
				.iter()
				.any(|filter| !filter.is_empty());

				(
					fetch_type,
					subscriber_priority,
					group_order,
					range_filters,
					fill_timeout.is_some(),
				)
			}
			// Draft-20 names the track up front and moves the range into LOCATION_FILTER.
			_ => {
				let namespace = decode_namespace(buf)?;
				let track = Cow::Owned(buf.string()?);
				decode_params!(buf, version,
					0x03 => _authorization_token: Vec<Opaque>,
					0x0A => fill_timeout: Option<u64>,
					0x20 => subscriber_priority: Option<u8>,
					0x21 => filter: Option<Filter>,
					0x22 => group_order: Option<GroupOrder>,
					0x25 => subgroup_filter: Vec<Opaque>,
					0x26 => object_id_filter: Vec<Opaque>,
					0x27 => priority_filter: Vec<Opaque>,
					0x28 => object_property_filter: Vec<Opaque>,
					0x35 => _include_properties: Option<bool>,
				);
				let range_filters = [
					subgroup_filter,
					object_id_filter,
					priority_filter,
					object_property_filter,
				]
				.iter()
				.any(|filter| !filter.is_empty());

				let fetch_type = FetchType::Filtered {
					namespace,
					track,
					// An absent LOCATION_FILTER fetches the whole track.
					filter: filter.unwrap_or(Filter::Unfiltered),
				};
				(
					fetch_type,
					subscriber_priority,
					group_order,
					range_filters,
					fill_timeout.is_some(),
				)
			}
		};

		Ok(Self {
			request_id,
			subscriber_priority: subscriber_priority.unwrap_or(128),
			// No preference: the publisher picks the order.
			group_order: group_order.unwrap_or(GroupOrder::Any),
			fetch_type,
			range_filters,
			fill_timeout,
		})
	}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOk {
	pub request_id: Option<RequestId>,
	pub group_order: GroupOrder,
	pub end_of_track: bool,
	pub end_location: Location,
	/// The track's properties, as SUBSCRIBE_OK carries them: MAX_CACHE_DURATION from the
	/// draft-14/15 parameters, the Track Properties block from draft-16 on.
	pub properties: super::Properties,
}
impl Message for FetchOk {
	const ID: u64 = 0x18;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
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
				w.bool(self.end_of_track);
				self.end_location.encode(w, version)?;
				w.u8(0); // no parameters
			}
			_ => {
				// GROUP_ORDER is not a legal FETCH_OK parameter in any draft after 14; the order
				// of the response is whatever the FETCH asked for.
				w.bool(self.end_of_track);
				self.end_location.encode(w, version)?;
				encode_params!(w, version,);
				// Track Properties are the final field, so nothing may follow.
				self.properties.encode(w, version)?;
			}
		}
		Ok(())
	}

	fn decode_msg(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let request_id = if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			Some(RequestId::decode(buf, version)?)
		} else {
			None
		};

		match version {
			Version::Draft14 => {
				let group_order = GroupOrder::decode(buf, version)?;
				let end_of_track = buf.bool()?;
				let end_location = Location::decode(buf, version)?;
				let properties = super::Properties {
					max_cache_duration: Parameters::skip(buf)?.map(std::time::Duration::from_millis),
					..Default::default()
				};
				Ok(Self {
					request_id,
					group_order,
					end_of_track,
					end_location,
					properties,
				})
			}
			_ => {
				let end_of_track = buf.bool()?;
				let end_location = Location::decode(buf, version)?;
				// MAX_CACHE_DURATION and GROUP_ORDER are legal on FETCH_OK only in draft-15.
				// Draft-16 still knows GROUP_ORDER, so one here is ignored. From draft-17
				// it closes the session.
				decode_params!(buf, version,
					0x04 => max_cache_duration: Option<u64> where version == Version::Draft15,
					0x22 => group_order: Option<GroupOrder> where version == Version::Draft15,
				);
				// The timescale is read but not surfaced yet: a fetched object without its
				// own units arrives untimed.
				let mut properties = super::Properties::decode(buf, version)?;
				if version == Version::Draft15 {
					properties.max_cache_duration = max_cache_duration.map(std::time::Duration::from_millis);
				}

				let group_order = group_order.unwrap_or(GroupOrder::Descending);

				Ok(Self {
					request_id,
					group_order,
					end_of_track,
					end_location,
					properties,
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

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		w.varint(self.error_code)?;
		w.string(&self.reason_phrase)?;
		Ok(())
	}

	fn decode_msg(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(buf, version)?;
		let error_code = buf.varint()?;
		let reason_phrase = Cow::Owned(buf.string()?);
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

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
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
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}
}

impl Decode<Version> for FetchHeader {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
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
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		match self {
			Self::EndOfRange { reason, group, object } => {
				if !Self::END_OF_RANGE.contains(reason) {
					return Err(EncodeError::InvalidState);
				}
				w.varint(*reason)?;
				w.varint(*group)?;
				w.varint(*object)?;
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
				w.varint(flags)?;

				if let Some(group) = group {
					w.varint(*group)?;
				}
				if let FetchSubgroup::Explicit(subgroup) = subgroup {
					w.varint(*subgroup)?;
				}
				if let Some(object) = object {
					w.varint(*object)?;
				}
				if let Some(priority) = priority {
					w.u8(*priority);
				}
				if let Some(properties) = properties {
					w.bytes(properties)?;
				}
			}
		}
		Ok(())
	}
}

impl Decode<Version> for FetchObject {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let flags = buf.varint()?;

		// Anything at or above 128 is a named value rather than a set of flags, and only
		// the three End of Range markers are defined.
		if flags >= 0x80 {
			if !Self::END_OF_RANGE.contains(&flags) {
				return Err(DecodeError::InvalidValue);
			}
			return Ok(Self::EndOfRange {
				reason: flags,
				group: buf.varint()?,
				object: buf.varint()?,
			});
		}

		// Wire order: Group ID Delta, Subgroup ID, Object ID Delta, Priority, Properties.
		let group = match flags & flag::GROUP_ID != 0 {
			true => Some(buf.varint()?),
			false => None,
		};

		let subgroup = match flags & flag::DATAGRAM != 0 {
			true => FetchSubgroup::Datagram,
			false => match flags & flag::SUBGROUP {
				0 => FetchSubgroup::Zero,
				1 => FetchSubgroup::Prior,
				2 => FetchSubgroup::PriorPlusOne,
				_ => FetchSubgroup::Explicit(buf.varint()?),
			},
		};

		let object = match flags & flag::OBJECT_ID != 0 {
			true => Some(buf.varint()?),
			false => None,
		};

		let priority = match flags & flag::PRIORITY != 0 {
			true => Some(buf.u8()?),
			false => None,
		};

		let properties = match flags & flag::PROPERTIES != 0 {
			true => {
				let super::group::ObjectExtensionsLength(size) =
					super::group::ObjectExtensionsLength::decode(buf, version)?;
				Some(buf.slice(size)?.to_vec())
			}
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
			range_filters: false,
			fill_timeout: false,
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
			range_filters: false,
			fill_timeout: false,
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
			properties: Default::default(),
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
			range_filters: false,
			fill_timeout: false,
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
			range_filters: false,
			fill_timeout: false,
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
			properties: Default::default(),
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
			properties: Default::default(),
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
			properties: Default::default(),
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
			range_filters: false,
			fill_timeout: false,
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
			properties: Default::default(),
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
			properties: Default::default(),
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

	/// The head of a draft-20 FETCH (Figure 16) up to its Number of Parameters: Request
	/// ID 1, Track Namespace ("live"), Track Name ("video"). There is no Fetch Type.
	const FETCH_HEAD: &[u8] = &[
		0x01, 0x01, 0x04, b'l', b'i', b'v', b'e', 0x05, b'v', b'i', b'd', b'e', b'o',
	];

	/// A draft-20 FETCH names the track up front and carries its range in LOCATION_FILTER.
	/// Reading a Fetch Type there instead takes the namespace's field count for one.
	#[test]
	fn test_fetch_v20_decodes_every_parameter() {
		#[rustfmt::skip]
		let body = [FETCH_HEAD, &[
			0x07, // Number of Parameters
			0x03, 0x03, 0x03, 0x00, 0xAA, // AUTHORIZATION TOKEN
			0x07, 0x64, // FILL_TIMEOUT (0x0A) = 100, a varint
			0x16, 0x40, // SUBSCRIBER_PRIORITY (0x20) = 64
			0x01, 0x03, 0x04, 0x00, 0x02, // LOCATION_FILTER (0x21): groups 4 through 6
			0x01, 0x01, // GROUP_ORDER (0x22) = Ascending
			0x04, 0x02, 0x00, 0x05, // OBJECTID_FILTER (0x26): SetID 0, from 5
			0x0F, 0x00, // INCLUDE_PROPERTIES (0x35) = 0
		]].concat();

		for version in [Version::Draft20, Version::Draft21, Version::Draft22] {
			let fetch: Fetch = decode_message(&body, version).unwrap_or_else(|e| panic!("{version}: {e}"));
			assert_eq!(fetch.request_id, RequestId(1));
			assert_eq!(fetch.subscriber_priority, 64);
			assert_eq!(fetch.group_order, GroupOrder::Ascending);
			assert!(fetch.range_filters, "{version}");
			assert!(fetch.fill_timeout, "{version}");
			assert_eq!(
				fetch.fetch_type,
				FetchType::Filtered {
					namespace: Path::new("live"),
					track: "video".into(),
					filter: Filter::Absolute {
						start: Location { group: 4, object: 0 },
						end: Some(crate::ietf::EndLocation { group: 6, object: None }),
					},
				},
				"{version}"
			);
		}
	}

	/// With no LOCATION_FILTER a draft-20 FETCH is the whole track, and that is what we
	/// write back: the parameter is omitted rather than sent empty.
	#[test]
	fn test_fetch_v20_wire() {
		let fetch = Fetch {
			request_id: RequestId(1),
			subscriber_priority: 128,
			group_order: GroupOrder::Descending,
			fetch_type: FetchType::Filtered {
				namespace: Path::new("live"),
				track: "video".into(),
				filter: Filter::Unfiltered,
			},
			range_filters: false,
			fill_timeout: false,
		};

		#[rustfmt::skip]
		let expected = [FETCH_HEAD, &[
			0x02, // Number of Parameters
			0x20, 0x80, // SUBSCRIBER_PRIORITY = 128
			0x02, 0x02, // GROUP_ORDER (0x22) = Descending
		]].concat();
		assert_eq!(encode_message(&fetch, Version::Draft20), expected);
		assert_eq!(decode_message::<Fetch>(&expected, Version::Draft20).unwrap(), fetch);

		// The tagged forms have no spelling from draft-20 on, and this one none before it.
		let mut buf = Vec::new();
		assert!(
			fetch
				.encode_msg(&mut Encoder::new(&mut buf, Version::Draft19.into()), Version::Draft19)
				.is_err()
		);
	}

	/// FILL_TIMEOUT arrived in draft-18 and the Range Filters in draft-19; each is still
	/// an unknown parameter before then.
	#[test]
	fn test_fetch_parameters_follow_their_draft() {
		#[rustfmt::skip]
		let joining = |params: &[u8]| [&[
			0x01, // Request ID
			0x02, 0x03, 0x00, // Relative Joining: subscription 3, offset 0
		][..], params].concat();

		let token = joining(&[0x01, 0x03, 0x03, 0x03, 0x00, 0xAA]);
		let fill_timeout = joining(&[0x01, 0x0A, 0x20]);
		let range_filter = joining(&[0x01, 0x25, 0x00]);

		for (version, body, ok) in [
			(Version::Draft15, &token, true),
			(Version::Draft16, &fill_timeout, false),
			(Version::Draft18, &fill_timeout, true),
			(Version::Draft18, &range_filter, false),
			(Version::Draft19, &range_filter, true),
		] {
			let decoded = decode_message::<Fetch>(body, version);
			assert_eq!(decoded.is_ok(), ok, "{version}: {body:x?}");
			if let Ok(fetch) = decoded {
				assert_eq!(fetch.range_filters, body == &range_filter, "{version}");
				assert_eq!(fetch.fill_timeout, body == &fill_timeout, "{version}");
			}
		}
	}
}

/// The Object serialization on a fetch stream (draft-20 section 11.4.4), which a fill's
/// head arrives on.
#[cfg(test)]
mod object_tests {
	use super::*;
	use bytes::Buf as _;

	const VERSION: Version = Version::Draft20;

	fn round_trip(object: &FetchObject) -> (Vec<u8>, FetchObject) {
		let mut buf = Vec::new();
		object
			.encode(&mut Encoder::new(&mut buf, VERSION.into()), VERSION)
			.expect("encode");

		let mut bytes = bytes::Bytes::from(buf.to_vec());
		let decoded = crate::coding::decode_buf(&mut bytes, VERSION, FetchObject::decode).expect("decode");
		assert!(!bytes.has_remaining(), "the object header is fully consumed");

		(buf.to_vec(), decoded)
	}

	/// A fetch stream is read until a whole object header has arrived, so a properties
	/// length the peer declares is refused at its prefix rather than buffered.
	#[test]
	fn oversized_properties_are_refused_at_the_prefix() {
		let mut wire = Vec::new();
		Encoder::new(&mut wire, VERSION.into())
			.varint(flag::PROPERTIES)
			.unwrap();
		Encoder::new(&mut wire, VERSION.into())
			.varint((super::super::group::MAX_OBJECT_EXTENSIONS + 1) as u64)
			.unwrap();

		let err = FetchObject::decode_slice(&wire, VERSION).unwrap_err();
		assert!(matches!(err, DecodeError::MessageTooLarge { .. }), "{err:?}");
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
		assert!(crate::coding::decode_buf(&mut bytes, VERSION, FetchObject::decode).is_err());
	}
}

#[cfg(test)]
mod cache_duration_tests {
	use super::*;
	use std::time::Duration;

	/// FETCH_OK carries MAX_CACHE_DURATION where SUBSCRIBE_OK does, so a track fetched with no
	/// subscription still learns its retention window.
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
					Version::Draft14 => vec![0, 1, 0, 0, 0, 1, 4],
					Version::Draft15 => vec![0, 0, 0, 0, 1, 4],
					Version::Draft16 => vec![0, 0, 0, 0, 0, 4],
					_ => vec![0, 0, 0, 0, 4],
				};
				Encoder::new(&mut payload, version.into()).varint(age).unwrap();
				let got = FetchOk::decode_msg(&mut Decoder::new(&payload, version.into()), version).unwrap();
				assert_eq!(
					got.properties.max_cache_duration,
					Some(Duration::from_millis(age)),
					"{version}"
				);
			}
		}
	}
}
