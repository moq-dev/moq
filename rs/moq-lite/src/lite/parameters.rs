use std::collections::HashMap;

use crate::coding::*;

use super::Version;

const MAX_PARAMS: u64 = 64;

#[derive(Default, Debug, Clone)]
pub struct Parameters(HashMap<u64, Vec<u8>>);

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
