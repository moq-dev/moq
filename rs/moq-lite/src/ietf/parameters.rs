use std::collections::{hash_map, HashMap};

use crate::coding::*;

const MAX_PARAMS: u64 = 64;

#[derive(Default, Debug, Clone)]
pub struct Parameters {
	ints: HashMap<u64, u64>,
	bytes: HashMap<u64, Vec<u8>>,
}

impl<V: Clone> Decode<V> for Parameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: V) -> Result<Self, DecodeError> {
		let mut ints = HashMap::new();
		let mut bytes = HashMap::new();

		// I hate this encoding so much; let me encode my role and get on with my life.
		let count = u64::decode(r, version.clone())?;

		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		for _ in 0..count {
			let kind = u64::decode(r, version.clone())?;

			if kind % 2 == 0 {
				match ints.entry(kind) {
					hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
					hash_map::Entry::Vacant(entry) => entry.insert(u64::decode(&mut r, version.clone())?),
				};
			} else {
				match bytes.entry(kind) {
					hash_map::Entry::Occupied(_) => return Err(DecodeError::Duplicate),
					hash_map::Entry::Vacant(entry) => entry.insert(Vec::<u8>::decode(&mut r, version.clone())?),
				};
			}
		}

		Ok(Parameters { ints, bytes })
	}
}

impl<V: Clone> Encode<V> for Parameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		(self.ints.len() + self.bytes.len()).encode(w, version.clone());

		for (kind, value) in self.ints.iter() {
			kind.encode(w, version.clone());
			value.encode(w, version.clone());
		}

		for (kind, value) in self.bytes.iter() {
			kind.encode(w, version.clone());
			value.encode(w, version.clone());
		}
	}
}

impl Parameters {
	pub fn get_int(&self, kind: u64) -> Option<u64> {
		assert!(kind.is_multiple_of(2));
		self.ints.get(&kind).copied()
	}

	pub fn set_int(&mut self, kind: u64, value: u64) {
		assert!(kind.is_multiple_of(2));
		self.ints.insert(kind, value);
	}

	pub fn get_bytes(&self, kind: u64) -> Option<&[u8]> {
		assert!(!kind.is_multiple_of(2));
		self.bytes.get(&kind).map(|v| v.as_slice())
	}

	pub fn set_bytes(&mut self, kind: u64, value: Vec<u8>) {
		assert!(!kind.is_multiple_of(2));
		self.bytes.insert(kind, value);
	}
}
