use crate::coding::*;

use super::Version;

const MAX_PARAMS: u64 = 64;

/// A bag of `id -> raw bytes` parameters, the body shared by SETUP (and any other
/// parameterized message). Encoded as a varint count followed by `id, length, value`
/// triples; duplicate ids are rejected on decode.
///
/// A handful at most, so a linear scan beats hashing, and the encoding keeps the
/// order the parameters were set or decoded in.
#[derive(Default, Debug, Clone)]
pub struct Parameters(Vec<(u64, Vec<u8>)>);

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

	/// Set a parameter to a varint value, replacing any existing entry.
	///
	/// Panics past [`crate::coding::varint::MAX_QUIC`], which no parameter we set comes near.
	pub fn set_varint(&mut self, id: u64, value: u64) {
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Form::Quic)
			.varint(value)
			.expect("parameter varint in range");
		self.set_bytes(id, buf);
	}

	/// Decode a parameter as a single varint, if present. Errors if trailing bytes remain.
	pub fn get_varint(&self, id: u64) -> Result<Option<u64>, DecodeError> {
		let Some(bytes) = self.get_bytes(id) else {
			return Ok(None);
		};
		let mut r = Decoder::new(bytes, Form::Quic);
		let value = r.varint()?;
		if !r.is_empty() {
			return Err(DecodeError::Long);
		}
		Ok(Some(value))
	}
}

impl Decode<Version> for Parameters {
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let mut params = Self::default();

		// I hate this encoding so much; let me encode my role and get on with my life.
		let count = r.varint()?;
		if count > MAX_PARAMS {
			return Err(DecodeError::TooMany);
		}

		for _ in 0..count {
			let kind = r.varint()?;
			if params.get_bytes(kind).is_some() {
				return Err(DecodeError::Duplicate);
			}

			params.0.push((kind, r.bytes()?.to_vec()));
		}

		Ok(params)
	}
}

impl Encode<Version> for Parameters {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		if self.0.len() as u64 > MAX_PARAMS {
			return Err(EncodeError::TooMany);
		}

		w.varint(self.0.len() as u64)?;

		for (kind, value) in &self.0 {
			w.varint(*kind)?;
			w.bytes(value)?;
		}

		Ok(())
	}
}
