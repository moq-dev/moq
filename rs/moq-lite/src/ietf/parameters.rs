use std::collections::{BTreeMap, HashMap, btree_map, hash_map};

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::coding::*;

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

impl<V: Clone> Decode<V> for Parameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: V) -> Result<Self, DecodeError> {
		let mut vars = HashMap::new();
		let mut bytes = HashMap::new();

		// I hate this encoding so much; let me encode my role and get on with my life.
		let count = u64::decode(r, version.clone())?;

		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		for _ in 0..count {
			let kind = u64::decode(r, version.clone())?;

			if kind % 2 == 0 {
				let kind = ParameterVarInt::from(kind);
				match vars.entry(kind) {
					hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
					hash_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version.clone())?),
				};
			} else {
				let kind = ParameterBytes::from(kind);
				match bytes.entry(kind) {
					hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
					hash_map::Entry::Vacant(entry) => entry.insert(Vec::<u8>::decode(&mut r, version.clone())?),
				};
			}
		}

		Ok(Parameters { vars, bytes })
	}
}

impl<V: Clone> Encode<V> for Parameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		(self.vars.len() + self.bytes.len()).encode(w, version.clone());

		for (kind, value) in self.vars.iter() {
			u64::from(*kind).encode(w, version.clone());
			value.encode(w, version.clone());
		}

		for (kind, value) in self.bytes.iter() {
			u64::from(*kind).encode(w, version.clone());
			value.encode(w, version.clone());
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

impl<V: Clone> Decode<V> for MessageParameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: V) -> Result<Self, DecodeError> {
		let mut vars = BTreeMap::new();
		let mut bytes = BTreeMap::new();

		let count = u64::decode(r, version.clone())?;

		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		for _ in 0..count {
			let kind = u64::decode(r, version.clone())?;

			if kind % 2 == 0 {
				match vars.entry(kind) {
					btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
					btree_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version.clone())?),
				};
			} else {
				match bytes.entry(kind) {
					btree_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
					btree_map::Entry::Vacant(entry) => entry.insert(Vec::<u8>::decode(&mut r, version.clone())?),
				};
			}
		}

		Ok(MessageParameters { vars, bytes })
	}
}

impl<V: Clone> Encode<V> for MessageParameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		(self.vars.len() + self.bytes.len()).encode(w, version.clone());

		for (kind, value) in self.vars.iter() {
			kind.encode(w, version.clone());
			value.encode(w, version.clone());
		}

		for (kind, value) in self.bytes.iter() {
			kind.encode(w, version.clone());
			value.encode(w, version.clone());
		}
	}
}

impl MessageParameters {
	// Varint parameter IDs (even)
	const DELIVERY_TIMEOUT: u64 = 0x02;
	const MAX_CACHE_DURATION: u64 = 0x04;
	const EXPIRES: u64 = 0x08;
	const PUBLISHER_PRIORITY: u64 = 0x0E;
	const FORWARD: u64 = 0x10;
	const SUBSCRIBER_PRIORITY: u64 = 0x20;
	const GROUP_ORDER: u64 = 0x22;

	// Bytes parameter IDs (odd)
	#[allow(dead_code)]
	const AUTHORIZATION_TOKEN: u64 = 0x03;
	const LARGEST_OBJECT: u64 = 0x09;
	const SUBSCRIPTION_FILTER: u64 = 0x21;

	// --- Varint accessors ---

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
