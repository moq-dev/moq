use std::collections::HashSet;

use crate::coding::*;

use super::Version;

/// A bag of `id -> raw bytes` parameters, the body shared by SETUP (and any other
/// parameterized message). Encoded as a varint count followed by `id, length, value`
/// triples; duplicate ids are rejected on decode.
///
/// The encoding keeps the order the parameters were set or decoded in.
#[derive(Default, Clone)]
pub struct Parameters(Vec<(u64, Vec<u8>)>);

impl std::fmt::Debug for Parameters {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_map()
			.entries(self.0.iter().map(|(kind, value)| (kind, value.len())))
			.finish()
	}
}

impl Parameters {
	/// Set a parameter to a raw byte value, replacing any existing entry.
	pub fn set_bytes(&mut self, id: u64, value: Vec<u8>) {
		match self.0.iter_mut().find(|(k, _)| *k == id) {
			Some((_, v)) => *v = value,
			None => self.0.push((id, value)),
		}
	}

	/// Borrow a parameter's raw byte value, if present.
	pub fn get_bytes(&self, id: u64) -> Option<&[u8]> {
		self.0.iter().find(|(k, _)| *k == id).map(|(_, v)| v.as_slice())
	}

	/// Set a parameter to a varint value in `version`'s encoding, replacing any existing entry.
	pub fn set_varint(&mut self, id: u64, value: u64, version: Version) -> Result<(), EncodeError> {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, version.into()).varint(value)?;
		self.set_bytes(id, buf);
		Ok(())
	}

	/// Decode a parameter as a single varint in `version`'s encoding, if present. Errors if
	/// trailing bytes remain.
	pub fn get_varint(&self, id: u64, version: Version) -> Result<Option<u64>, DecodeError> {
		let Some(bytes) = self.get_bytes(id) else {
			return Ok(None);
		};
		let mut r = Decoder::new(bytes, version.into());
		let value = r.varint()?;
		if !r.is_empty() {
			return Err(DecodeError::Long);
		}
		Ok(Some(value))
	}
}

impl<'a> Decode<'a, Version> for Parameters {
	fn decode(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let mut params = Self::default();

		// I hate this encoding so much; let me encode my role and get on with my life.
		let count = r.varint()?;
		// Grow only as entries arrive; the enclosing message bounds the bag.
		// Linear lookup avoids hashing and another allocation for small SETUPs.
		// Larger bags need a set so duplicate detection cannot grow quadratically.
		let mut seen = (count > 64).then(HashSet::new);
		for _ in 0..count {
			let kind = r.varint()?;
			let duplicate = match &mut seen {
				Some(seen) => !seen.insert(kind),
				None => params.get_bytes(kind).is_some(),
			};
			if duplicate {
				return Err(DecodeError::Duplicate);
			}

			params.0.push((kind, r.bytes()?.to_vec()));
		}

		Ok(params)
	}
}

impl Encode<Version> for Parameters {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(self.0.len() as u64)?;

		for (kind, value) in &self.0 {
			w.varint(*kind)?;
			w.bytes(value)?;
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn large_bags_preserve_order_and_reject_duplicates() {
		let version = Version::Lite07;
		let mut params = Parameters::default();
		for kind in 100..1124 {
			params.set_bytes(kind, Vec::new());
		}
		let encoded = params.encode_bytes(version).unwrap();
		let mut reader = Decoder::new(&encoded, version.into());
		assert_eq!(Parameters::decode(&mut reader, version).unwrap().0, params.0);

		let mut duplicate = Vec::new();
		let mut writer = Encoder::new(&mut duplicate, version.into());
		writer.varint(1025).unwrap();
		for kind in (100..1124).chain([100]) {
			writer.varint(kind).unwrap();
			writer.bytes(&[]).unwrap();
		}
		assert!(matches!(
			Parameters::decode(&mut Decoder::new(&duplicate, version.into()), version),
			Err(DecodeError::Duplicate)
		));
	}

	#[test]
	fn a_declared_count_does_not_allocate_before_its_entries_arrive() {
		let version = Version::Lite07;
		let mut data = Vec::new();
		Encoder::new(&mut data, version.into()).varint(u64::MAX).unwrap();
		assert!(matches!(
			Parameters::decode(&mut Decoder::new(&data, version.into()), version),
			Err(DecodeError::Short)
		));
	}
}
