use std::collections::HashMap;

use crate::coding::*;

use super::Version;

const MAX_PARAMS: u64 = 64;

/// A bag of `id -> raw bytes` parameters, the body shared by SETUP (and any other
/// parameterized message). Encoded as a varint count followed by `id, length, value`
/// triples; duplicate ids are rejected on decode.
#[derive(Default, Debug, Clone)]
pub struct Parameters(HashMap<u64, Vec<u8>>);

impl Parameters {
	/// Set a parameter to a raw byte value, replacing any existing entry.
	pub fn set_bytes(&mut self, id: u64, value: Vec<u8>) {
		self.0.insert(id, value);
	}

	/// Borrow a parameter's raw byte value, if present.
	pub fn get_bytes(&self, id: u64) -> Option<&[u8]> {
		self.0.get(&id).map(Vec::as_slice)
	}

	/// Set a parameter to a varint value, replacing any existing entry.
	pub fn set_varint(&mut self, id: u64, value: u64) {
		let mut buf = Vec::new();
		// Infallible: writing into a Vec never runs short.
		value.encode(&mut buf, Version::Lite05).expect("varint encode into Vec");
		self.0.insert(id, buf);
	}

	/// Decode a parameter as a single varint, if present. Errors if trailing bytes remain.
	pub fn get_varint(&self, id: u64) -> Result<Option<u64>, DecodeError> {
		let Some(mut bytes) = self.0.get(&id).map(Vec::as_slice) else {
			return Ok(None);
		};
		let value = u64::decode(&mut bytes, Version::Lite05)?;
		if !bytes.is_empty() {
			return Err(DecodeError::Long);
		}
		Ok(Some(value))
	}
}

impl Decode<Version> for Parameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let mut map = HashMap::new();

		// I hate this encoding so much; let me encode my role and get on with my life.
		let count = u64::decode(r, version)?;
		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		for _ in 0..count {
			let kind = u64::decode(r, version)?;
			if map.contains_key(&kind) {
				return Err(DecodeError::Duplicate);
			}

			let data = Vec::<u8>::decode(&mut r, version)?;
			map.insert(kind, data);
		}

		Ok(Parameters(map))
	}
}

impl Encode<Version> for Parameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if self.0.len() as u64 > MAX_PARAMS {
			return Err(EncodeError::TooMany);
		}

		self.0.len().encode(w, version)?;

		for (kind, value) in self.0.iter() {
			kind.encode(w, version)?;
			value.encode(w, version)?;
		}

		Ok(())
	}
}
