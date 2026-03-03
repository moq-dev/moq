use std::collections::{BTreeMap, HashMap, btree_map, hash_map};

use bytes::Buf;
use num_enum::{FromPrimitive, IntoPrimitive};

use crate::coding::*;

use super::Version;

const MAX_PARAMS: u64 = 64;
const PARAM_SUBVALUE_VERSION: Version = Version::Draft15;

// ---- Setup Parameters (used in CLIENT_SETUP/SERVER_SETUP) ----

#[derive(Debug, Copy, Clone, FromPrimitive, IntoPrimitive, Eq, Hash, PartialEq)]
#[repr(u64)]
pub enum ParameterVarInt {
	MaxRequestId = 2,
	MaxAuthTokenCacheSize = 4,
	#[num_enum(catch_all)]
	Unknown(u64),
}

#[derive(Debug, Copy, Clone, FromPrimitive, IntoPrimitive, Eq, Hash, PartialEq)]
#[repr(u64)]
pub enum ParameterBytes {
	Path = 1,
	AuthorizationToken = 3,
	Authority = 5,
	Implementation = 7,
	#[num_enum(catch_all)]
	Unknown(u64),
}

#[derive(Default, Debug, Clone)]
pub struct Parameters {
	vars: HashMap<ParameterVarInt, u64>,
	bytes: HashMap<ParameterBytes, Vec<u8>>,
}

impl Decode<Version> for Parameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let mut vars = HashMap::new();
		let mut bytes = HashMap::new();

		match version {
			Version::Draft17 => {
				// Draft17: no count prefix, read Key-Value-Pairs until buffer empty.
				// Delta-encoded types, even = varint value, odd = length-prefixed bytes.
				let mut prev_type: u64 = 0;
				let mut i = 0u64;
				while r.has_remaining() {
					if i >= MAX_PARAMS {
						return Err(DecodeError::TooMany);
					}
					let delta = u64::decode(&mut r, version)?;
					let abs = if i == 0 { delta } else { prev_type + delta };
					prev_type = abs;
					i += 1;

					if abs % 2 == 0 {
						let kind = ParameterVarInt::from(abs);
						match vars.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version)?),
						};
					} else {
						let kind = ParameterBytes::from(abs);
						match bytes.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(Vec::<u8>::decode(&mut r, version)?),
						};
					}
				}
			}
			_ => {
				let count = u64::decode(r, version)?;

				if count > MAX_PARAMS {
					return Err(DecodeError::TooMany);
				}

				let mut prev_type: u64 = 0;

				for i in 0..count {
					let kind = match version {
						Version::Draft16 => {
							let delta = u64::decode(r, version)?;
							let abs = if i == 0 { delta } else { prev_type + delta };
							prev_type = abs;
							abs
						}
						Version::Draft14 | Version::Draft15 | Version::Draft17 => u64::decode(r, version)?,
					};

					if kind % 2 == 0 {
						let kind = ParameterVarInt::from(kind);
						match vars.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version)?),
						};
					} else {
						let kind = ParameterBytes::from(kind);
						match bytes.entry(kind) {
							hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							hash_map::Entry::Vacant(entry) => entry.insert(Vec::<u8>::decode(&mut r, version)?),
						};
					}
				}
			}
		}

		Ok(Parameters { vars, bytes })
	}
}

impl Encode<Version> for Parameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let count = self.vars.len() + self.bytes.len();
		if count as u64 > MAX_PARAMS {
			return Err(EncodeError::TooMany);
		}

		match version {
			Version::Draft16 | Version::Draft17 => {
				// Draft16: count prefix + delta encoding
				// Draft17: NO count prefix + delta encoding
				if version != Version::Draft17 {
					count.encode(w, version)?;
				}

				// Collect all keys, sort, encode deltas
				enum ParamRef<'a> {
					Var(&'a u64),
					Bytes(&'a Vec<u8>),
				}
				let mut all: Vec<(u64, ParamRef)> = Vec::new();
				for (k, v) in self.vars.iter() {
					all.push((u64::from(*k), ParamRef::Var(v)));
				}
				for (k, v) in self.bytes.iter() {
					all.push((u64::from(*k), ParamRef::Bytes(v)));
				}
				all.sort_by_key(|(k, _)| *k);

				let mut prev_type: u64 = 0;
				for (idx, (kind, val)) in all.iter().enumerate() {
					let delta = if idx == 0 { *kind } else { kind - prev_type };
					prev_type = *kind;
					delta.encode(w, version)?;

					match val {
						ParamRef::Var(v) => v.encode(w, version)?,
						ParamRef::Bytes(v) => v.encode(w, version)?,
					}
				}
			}
			Version::Draft14 | Version::Draft15 => {
				count.encode(w, version)?;

				for (kind, value) in self.vars.iter() {
					u64::from(*kind).encode(w, version)?;
					value.encode(w, version)?;
				}

				for (kind, value) in self.bytes.iter() {
					u64::from(*kind).encode(w, version)?;
					value.encode(w, version)?;
				}
			}
		}

		Ok(())
	}
}

impl Parameters {
	pub fn get_varint(&self, kind: ParameterVarInt) -> Option<u64> {
		self.vars.get(&kind).copied()
	}

	pub fn set_varint(&mut self, kind: ParameterVarInt, value: u64) {
		self.vars.insert(kind, value);
	}

	#[cfg(test)]
	pub fn get_bytes(&self, kind: ParameterBytes) -> Option<&[u8]> {
		self.bytes.get(&kind).map(|v| v.as_slice())
	}

	pub fn set_bytes(&mut self, kind: ParameterBytes, value: Vec<u8>) {
		self.bytes.insert(kind, value);
	}
}

// ---- Message Parameters (used in Subscribe, Publish, Fetch, etc.) ----
// Uses raw u64 keys since parameter IDs have different meanings from setup parameters.
// BTreeMap ensures deterministic wire encoding order.

#[derive(Default, Debug, Clone)]
pub struct MessageParameters {
	vars: BTreeMap<u64, u64>,
	bytes: BTreeMap<u64, Vec<u8>>,
}

impl Decode<Version> for MessageParameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let mut vars = BTreeMap::new();
		let mut bytes = BTreeMap::new();

		let count = u64::decode(r, version)?;

		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		let mut prev_type: u64 = 0;

		for i in 0..count {
			let kind = match version {
				Version::Draft16 | Version::Draft17 => {
					let delta = u64::decode(r, version)?;
					let abs = if i == 0 { delta } else { prev_type + delta };
					prev_type = abs;
					abs
				}
				Version::Draft14 | Version::Draft15 => u64::decode(r, version)?,
			};

			match version {
				Version::Draft17 => {
					// Type-specific value encoding for draft-17
					match kind {
						// uint8 types
						0x10 | 0x20 | 0x22 => {
							let val = u8::decode(&mut r, version)? as u64;
							match vars.entry(kind) {
								btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
								btree_map::Entry::Vacant(entry) => entry.insert(val),
							};
						}
						// varint types
						0x02 | 0x04 | 0x08 | 0x32 => {
							let val = u64::decode(&mut r, version)?;
							match vars.entry(kind) {
								btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
								btree_map::Entry::Vacant(entry) => entry.insert(val),
							};
						}
						// Location type (0x09 LARGEST_OBJECT): two consecutive varints
						0x09 => {
							let group = u64::decode(&mut r, version)?;
							let object = u64::decode(&mut r, version)?;
							// Store as internal bytes format (QUIC varint sub-values)
							let mut buf = Vec::new();
							let v = PARAM_SUBVALUE_VERSION;
							group.encode(&mut buf, v).map_err(|_| DecodeError::InvalidValue)?;
							object.encode(&mut buf, v).map_err(|_| DecodeError::InvalidValue)?;
							match bytes.entry(kind) {
								btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
								btree_map::Entry::Vacant(entry) => entry.insert(buf),
							};
						}
						// Length-prefixed bytes types (0x03, 0x21, and unknown odd types)
						_ if kind % 2 == 1 => {
							match bytes.entry(kind) {
								btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
								btree_map::Entry::Vacant(entry) => entry.insert(Vec::<u8>::decode(&mut r, version)?),
							};
						}
						// Unknown even types: varint
						_ => {
							let val = u64::decode(&mut r, version)?;
							match vars.entry(kind) {
								btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
								btree_map::Entry::Vacant(entry) => entry.insert(val),
							};
						}
					}
				}
				_ => {
					if kind % 2 == 0 {
						match vars.entry(kind) {
							btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							btree_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version)?),
						};
					} else {
						match bytes.entry(kind) {
							btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
							btree_map::Entry::Vacant(entry) => entry.insert(Vec::<u8>::decode(&mut r, version)?),
						};
					}
				}
			}
		}

		Ok(MessageParameters { vars, bytes })
	}
}

impl Encode<Version> for MessageParameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let count = self.vars.len() + self.bytes.len();
		if count as u64 > MAX_PARAMS {
			return Err(EncodeError::TooMany);
		}
		count.encode(w, version)?;

		match version {
			Version::Draft16 | Version::Draft17 => {
				// Delta encoding: BTreeMap is already sorted, merge and sort by key
				enum ParamValue<'a> {
					Var(&'a u64),
					Bytes(&'a Vec<u8>),
				}
				let mut all: Vec<(u64, ParamValue)> = Vec::new();
				for (k, v) in self.vars.iter() {
					all.push((*k, ParamValue::Var(v)));
				}
				for (k, v) in self.bytes.iter() {
					all.push((*k, ParamValue::Bytes(v)));
				}
				all.sort_by_key(|(k, _)| *k);

				let mut prev_type: u64 = 0;
				for (idx, (kind, val)) in all.iter().enumerate() {
					let delta = if idx == 0 { *kind } else { kind - prev_type };
					prev_type = *kind;
					delta.encode(w, version)?;

					match (version, val) {
						(Version::Draft17, ParamValue::Var(v)) => {
							// Type-specific value encoding for draft-17
							match *kind {
								// uint8 types
								0x10 | 0x20 | 0x22 => {
									let byte = u8::try_from(**v).map_err(|_| EncodeError::BoundsExceeded)?;
									byte.encode(w, version)?;
								}
								// varint types (including unknown even)
								_ => v.encode(w, version)?,
							}
						}
						(Version::Draft17, ParamValue::Bytes(v)) => {
							// Type-specific value encoding for draft-17
							match *kind {
								// Location type (0x09 LARGEST_OBJECT): two raw varints
								0x09 => {
									// Decode from internal bytes (QUIC varint sub-values)
									let mut buf = bytes::Bytes::from((*v).clone());
									let sv = PARAM_SUBVALUE_VERSION;
									let group = u64::decode(&mut buf, sv).map_err(|_| EncodeError::InvalidState)?;
									let object = u64::decode(&mut buf, sv).map_err(|_| EncodeError::InvalidState)?;
									// Write as two raw varints in draft-17 format
									group.encode(w, version)?;
									object.encode(w, version)?;
								}
								// Length-prefixed bytes
								_ => v.encode(w, version)?,
							}
						}
						(_, ParamValue::Var(v)) => v.encode(w, version)?,
						(_, ParamValue::Bytes(v)) => v.encode(w, version)?,
					}
				}
			}
			Version::Draft14 | Version::Draft15 => {
				for (kind, value) in self.vars.iter() {
					kind.encode(w, version)?;
					value.encode(w, version)?;
				}

				for (kind, value) in self.bytes.iter() {
					kind.encode(w, version)?;
					value.encode(w, version)?;
				}
			}
		}

		Ok(())
	}
}

impl MessageParameters {
	// Varint parameter IDs (even)
	//const DELIVERY_TIMEOUT: u64 = 0x02;
	//const MAX_CACHE_DURATION: u64 = 0x04;
	//const EXPIRES: u64 = 0x08;
	//const PUBLISHER_PRIORITY: u64 = 0x0E;
	const FORWARD: u64 = 0x10;
	const SUBSCRIBER_PRIORITY: u64 = 0x20;
	const GROUP_ORDER: u64 = 0x22;

	// Bytes parameter IDs (odd)
	#[allow(dead_code)]
	const AUTHORIZATION_TOKEN: u64 = 0x03;
	const LARGEST_OBJECT: u64 = 0x09;
	const SUBSCRIPTION_FILTER: u64 = 0x21;

	// --- Varint accessors ---

	/*
	pub fn delivery_timeout(&self) -> Option<u64> {
		self.vars.get(&Self::DELIVERY_TIMEOUT).copied()
	}

	pub fn set_delivery_timeout(&mut self, v: u64) {
		self.vars.insert(Self::DELIVERY_TIMEOUT, v);
	}

	pub fn max_cache_duration(&self) -> Option<u64> {
		self.vars.get(&Self::MAX_CACHE_DURATION).copied()
	}

	pub fn set_max_cache_duration(&mut self, v: u64) {
		self.vars.insert(Self::MAX_CACHE_DURATION, v);
	}

	pub fn expires(&self) -> Option<u64> {
		self.vars.get(&Self::EXPIRES).copied()
	}

	pub fn set_expires(&mut self, v: u64) {
		self.vars.insert(Self::EXPIRES, v);
	}

	pub fn publisher_priority(&self) -> Option<u8> {
		self.vars.get(&Self::PUBLISHER_PRIORITY).map(|v| *v as u8)
	}

	pub fn set_publisher_priority(&mut self, v: u8) {
		self.vars.insert(Self::PUBLISHER_PRIORITY, v as u64);
	}
	*/

	pub fn forward(&self) -> Option<bool> {
		self.vars.get(&Self::FORWARD).map(|v| *v != 0)
	}

	pub fn set_forward(&mut self, v: bool) {
		self.vars.insert(Self::FORWARD, v as u64);
	}

	pub fn subscriber_priority(&self) -> Option<u8> {
		self.vars.get(&Self::SUBSCRIBER_PRIORITY).map(|v| *v as u8)
	}

	pub fn set_subscriber_priority(&mut self, v: u8) {
		self.vars.insert(Self::SUBSCRIBER_PRIORITY, v as u64);
	}

	pub fn group_order(&self) -> Option<u64> {
		self.vars.get(&Self::GROUP_ORDER).copied()
	}

	pub fn set_group_order(&mut self, v: u64) {
		self.vars.insert(Self::GROUP_ORDER, v);
	}

	// --- Bytes accessors ---

	/// Get largest object location (encoded as group_id varint + object_id varint)
	pub fn largest_object(&self) -> Option<super::Location> {
		let data = self.bytes.get(&Self::LARGEST_OBJECT)?;
		let mut buf = bytes::Bytes::from(data.clone());
		// Sub-values within parameters always use QUIC varint encoding.
		let v = PARAM_SUBVALUE_VERSION;
		let group = u64::decode(&mut buf, v).ok()?;
		let object = u64::decode(&mut buf, v).ok()?;
		Some(super::Location { group, object })
	}

	pub fn set_largest_object(&mut self, loc: &super::Location) -> Result<(), EncodeError> {
		let mut buf = Vec::new();
		// Sub-values within parameters always use QUIC varint encoding.
		let v = PARAM_SUBVALUE_VERSION;
		loc.group.encode(&mut buf, v)?;
		loc.object.encode(&mut buf, v)?;
		self.bytes.insert(Self::LARGEST_OBJECT, buf);
		Ok(())
	}

	/// Get subscription filter (encoded as filter_type varint [+ filter data])
	pub fn subscription_filter(&self) -> Option<super::FilterType> {
		let data = self.bytes.get(&Self::SUBSCRIPTION_FILTER)?;
		let mut buf = bytes::Bytes::from(data.clone());
		// Sub-values within parameters always use QUIC varint encoding.
		super::FilterType::decode(&mut buf, PARAM_SUBVALUE_VERSION).ok()
	}

	pub fn set_subscription_filter(&mut self, ft: super::FilterType) -> Result<(), EncodeError> {
		let mut buf = Vec::new();
		// Sub-values within parameters always use QUIC varint encoding.
		ft.encode(&mut buf, PARAM_SUBVALUE_VERSION)?;
		self.bytes.insert(Self::SUBSCRIPTION_FILTER, buf);
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::{Buf, BytesMut};

	#[test]
	fn test_parameters_v16_delta_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxRequestId, 100);
		params.set_bytes(ParameterBytes::Implementation, b"test-impl".to_vec());

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft16).unwrap();

		let mut bytes = buf.freeze();
		let decoded = Parameters::decode(&mut bytes, Version::Draft16).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxRequestId), Some(100));
		assert_eq!(
			decoded.get_bytes(ParameterBytes::Implementation),
			Some(b"test-impl".as_ref())
		);
	}

	#[test]
	fn test_parameters_v15_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxRequestId, 100);

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft15).unwrap();

		let mut bytes = buf.freeze();
		let decoded = Parameters::decode(&mut bytes, Version::Draft15).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxRequestId), Some(100));
	}

	#[test]
	fn test_message_parameters_v16_delta_round_trip() {
		let mut params = MessageParameters::default();
		params.set_subscriber_priority(200);
		params.set_group_order(2);
		params.set_forward(true);

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft16).unwrap();

		let mut bytes = buf.freeze();
		let decoded = MessageParameters::decode(&mut bytes, Version::Draft16).unwrap();

		assert_eq!(decoded.subscriber_priority(), Some(200));
		assert_eq!(decoded.group_order(), Some(2));
		assert_eq!(decoded.forward(), Some(true));
	}

	#[test]
	fn test_message_parameters_v15_round_trip() {
		let mut params = MessageParameters::default();
		params.set_subscriber_priority(128);
		params.set_group_order(2);

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft15).unwrap();

		let mut bytes = buf.freeze();
		let decoded = MessageParameters::decode(&mut bytes, Version::Draft15).unwrap();

		assert_eq!(decoded.subscriber_priority(), Some(128));
		assert_eq!(decoded.group_order(), Some(2));
	}

	#[test]
	fn test_message_parameters_v17_round_trip() {
		use crate::ietf::{FilterType, Location};

		let mut params = MessageParameters::default();
		params.set_subscriber_priority(200);
		params.set_group_order(2);
		params.set_forward(true);
		params.set_largest_object(&Location { group: 5, object: 3 }).unwrap();
		params.set_subscription_filter(FilterType::LargestObject).unwrap();

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft17).unwrap();

		let mut bytes = buf.freeze();
		let decoded = MessageParameters::decode(&mut bytes, Version::Draft17).unwrap();

		assert_eq!(decoded.subscriber_priority(), Some(200));
		assert_eq!(decoded.group_order(), Some(2));
		assert_eq!(decoded.forward(), Some(true));
		assert_eq!(decoded.largest_object(), Some(Location { group: 5, object: 3 }));
		assert_eq!(decoded.subscription_filter(), Some(FilterType::LargestObject));
	}

	#[test]
	fn test_parameters_v17_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxRequestId, 100);
		params.set_bytes(ParameterBytes::Implementation, b"test-impl".to_vec());

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft17).unwrap();

		let mut bytes = buf.freeze();
		let decoded = Parameters::decode(&mut bytes, Version::Draft17).unwrap();

		assert_eq!(decoded.get_bytes(ParameterBytes::Path), Some(b"/test".as_ref()));
		assert_eq!(decoded.get_varint(ParameterVarInt::MaxRequestId), Some(100));
		assert_eq!(
			decoded.get_bytes(ParameterBytes::Implementation),
			Some(b"test-impl".as_ref())
		);
		// Buffer should be fully consumed
		assert!(!bytes.has_remaining());
	}

	#[test]
	fn test_parameters_v17_no_count_prefix() {
		// Verify Draft17 Parameters have no count prefix by checking
		// that Draft17 encoding differs from Draft15 (which has a count prefix)
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/x".to_vec());

		let mut buf15 = BytesMut::new();
		params.encode(&mut buf15, Version::Draft15).unwrap();

		let mut buf17 = BytesMut::new();
		params.encode(&mut buf17, Version::Draft17).unwrap();

		// Draft17 should be shorter (no count prefix varint)
		assert!(buf17.len() < buf15.len());
	}

	#[test]
	fn test_message_parameters_empty_v16() {
		let params = MessageParameters::default();

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft16).unwrap();

		let mut bytes = buf.freeze();
		let decoded = MessageParameters::decode(&mut bytes, Version::Draft16).unwrap();

		assert_eq!(decoded.subscriber_priority(), None);
	}
}
