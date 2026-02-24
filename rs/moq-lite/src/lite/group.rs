use crate::{
	coding::*,
	lite::{Message, Version},
};

#[derive(Clone, Debug)]
pub struct Group {
	// The subscribe ID.
	pub subscribe: u64,

	// The group sequence number
	pub sequence: u64,
}

impl Message for Group {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Ok(Self {
			subscribe: u64::decode(r, version)?,
			sequence: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.subscribe.encode(w, version);
		self.sequence.encode(w, version);
	}
}
