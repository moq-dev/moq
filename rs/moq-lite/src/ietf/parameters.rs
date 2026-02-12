use std::collections::{BTreeMap, HashMap, btree_map, hash_map};

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::coding::*;

use super::Version;

const MAX_PARAMS: u64 = 64;

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
				_ => u64::decode(r, version)?,
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

		Ok(Parameters { vars, bytes })
	}
}

impl Encode<Version> for Parameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		(self.vars.len() + self.bytes.len()).encode(w, version);

		match version {
			Version::Draft16 => {
				// Delta encoding: collect all keys, sort, encode deltas
				let mut all: Vec<(u64, bool, usize)> = Vec::new(); // (key, is_var, index)
				let var_keys: Vec<_> = self.vars.keys().collect();
				let byte_keys: Vec<_> = self.bytes.keys().collect();
				for (i, k) in var_keys.iter().enumerate() {
					all.push((u64::from(**k), true, i));
				}
				for (i, k) in byte_keys.iter().enumerate() {
					all.push((u64::from(**k), false, i));
				}
				all.sort_by_key(|(k, _, _)| *k);

				let var_vals: Vec<_> = self.vars.values().collect();
				let byte_vals: Vec<_> = self.bytes.values().collect();

				let mut prev_type: u64 = 0;
				for (idx, (kind, is_var, orig_idx)) in all.iter().enumerate() {
					let delta = if idx == 0 { *kind } else { kind - prev_type };
					prev_type = *kind;
					delta.encode(w, version);

					if *is_var {
						var_vals[*orig_idx].encode(w, version);
					} else {
						byte_vals[*orig_idx].encode(w, version);
					}
				}
			}
			_ => {
				for (kind, value) in self.vars.iter() {
					u64::from(*kind).encode(w, version);
					value.encode(w, version);
				}

				for (kind, value) in self.bytes.iter() {
					u64::from(*kind).encode(w, version);
					value.encode(w, version);
				}
			}
		}
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
				Version::Draft16 => {
					let delta = u64::decode(r, version)?;
					let abs = if i == 0 { delta } else { prev_type + delta };
					prev_type = abs;
					abs
				}
				_ => u64::decode(r, version)?,
			};

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

		Ok(MessageParameters { vars, bytes })
	}
}

impl Encode<Version> for MessageParameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		(self.vars.len() + self.bytes.len()).encode(w, version);

		match version {
			Version::Draft16 => {
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
					delta.encode(w, version);

					match val {
						ParamValue::Var(v) => v.encode(w, version),
						ParamValue::Bytes(v) => v.encode(w, version),
					}
				}
			}
			_ => {
				for (kind, value) in self.vars.iter() {
					kind.encode(w, version);
					value.encode(w, version);
				}

				for (kind, value) in self.bytes.iter() {
					kind.encode(w, version);
					value.encode(w, version);
				}
			}
		}
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
		let group = u64::decode(&mut buf, ()).ok()?;
		let object = u64::decode(&mut buf, ()).ok()?;
		Some(super::Location { group, object })
	}

	pub fn set_largest_object(&mut self, loc: &super::Location) {
		let mut buf = Vec::new();
		loc.group.encode(&mut buf, ());
		loc.object.encode(&mut buf, ());
		self.bytes.insert(Self::LARGEST_OBJECT, buf);
	}

	/// Get subscription filter (encoded as filter_type varint [+ filter data])
	pub fn subscription_filter(&self) -> Option<super::FilterType> {
		let data = self.bytes.get(&Self::SUBSCRIPTION_FILTER)?;
		let mut buf = bytes::Bytes::from(data.clone());
		super::FilterType::decode(&mut buf, ()).ok()
	}

	pub fn set_subscription_filter(&mut self, ft: super::FilterType) {
		let mut buf = Vec::new();
		ft.encode(&mut buf, ());
		self.bytes.insert(Self::SUBSCRIPTION_FILTER, buf);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	#[test]
	fn test_parameters_v16_delta_round_trip() {
		let mut params = Parameters::default();
		params.set_bytes(ParameterBytes::Path, b"/test".to_vec());
		params.set_varint(ParameterVarInt::MaxRequestId, 100);
		params.set_bytes(ParameterBytes::Implementation, b"test-impl".to_vec());

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft16);

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
		params.encode(&mut buf, Version::Draft15);

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
		params.encode(&mut buf, Version::Draft16);

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
		params.encode(&mut buf, Version::Draft15);

		let mut bytes = buf.freeze();
		let decoded = MessageParameters::decode(&mut bytes, Version::Draft15).unwrap();

		assert_eq!(decoded.subscriber_priority(), Some(128));
		assert_eq!(decoded.group_order(), Some(2));
	}

	#[test]
	fn test_message_parameters_empty_v16() {
		let params = MessageParameters::default();

		let mut buf = BytesMut::new();
		params.encode(&mut buf, Version::Draft16);

		let mut bytes = buf.freeze();
		let decoded = MessageParameters::decode(&mut bytes, Version::Draft16).unwrap();

		assert_eq!(decoded.subscriber_priority(), None);
	}
}
