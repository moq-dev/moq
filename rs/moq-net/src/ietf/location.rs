use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::Version;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
	pub group: u64,
	pub object: u64,
}

impl Encode<Version> for Location {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(self.group)?;
		w.varint(self.object)?;
		Ok(())
	}
}

impl Decode<Version> for Location {
	fn decode(buf: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let group = buf.varint()?;
		let object = buf.varint()?;
		Ok(Self { group, object })
	}
}
