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

/// Indicates that one or more groups have been dropped.
///
/// Draft03 only.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct GroupDrop {
	pub sequence: u64,
	pub count: u64,
	pub error: u64,
}

impl Message for GroupDrop {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Ok(Self {
			sequence: u64::decode(r, version)?,
			count: u64::decode(r, version)?,
			error: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.sequence.encode(w, version);
		self.count.encode(w, version);
		self.error.encode(w, version);
	}
}
