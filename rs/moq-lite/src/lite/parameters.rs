use std::collections::HashMap;

use crate::coding::*;

const MAX_PARAMS: u64 = 64;

#[derive(Default, Debug, Clone)]
pub struct Parameters(HashMap<u64, Vec<u8>>);

impl<V: Clone> Decode<V> for Parameters {
	fn decode<R: bytes::Buf>(mut r: &mut R, version: V) -> Result<Self, DecodeError> {
		let mut map = HashMap::new();

		// I hate this encoding so much; let me encode my role and get on with my life.
		let count = u64::decode(r, version.clone())?;
		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		for _ in 0..count {
			let kind = u64::decode(r, version.clone())?;
			if map.contains_key(&kind) {
				return Err(DecodeError::Duplicate);
			}

			let data = Vec::<u8>::decode(&mut r, version.clone())?;
			map.insert(kind, data);
		}

		Ok(Parameters(map))
	}
}

impl<V: Clone> Encode<V> for Parameters {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.0.len().encode(w, version.clone())?;

		for (kind, value) in self.0.iter() {
			kind.encode(w, version.clone())?;
			value.encode(w, version.clone())?;
		}

		Ok(())
	}
}
