use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::Version;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
	pub group: u64,
	pub object: u64,
}

impl Encode<Version> for Location {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.group.encode(w, version)?;
		self.object.encode(w, version)?;
		Ok(())
	}
}

impl Decode<Version> for Location {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let group = u64::decode(buf, version)?;
		let object = u64::decode(buf, version)?;
		Ok(Self { group, object })
	}
}
