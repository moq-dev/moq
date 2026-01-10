use crate::{
	coding::{Decode, DecodeError, Encode},
	ietf::{Message, Version},
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct RequestId(pub u64);

impl RequestId {
	pub fn increment(&mut self) -> RequestId {
		let prev = self.0;
		self.0 += 2;
		RequestId(prev)
	}
}

impl std::fmt::Display for RequestId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0)
	}
}

impl<V> Encode<V> for RequestId {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.0.encode(w, version);
	}
}

impl<V> Decode<V> for RequestId {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let request_id = u64::decode(r, version)?;
		Ok(Self(request_id))
	}
}

#[derive(Clone, Debug)]
pub struct MaxRequestId {
	pub request_id: RequestId,
}

impl Message for MaxRequestId {
	const ID: u64 = 0x15;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

#[derive(Clone, Debug)]
pub struct RequestsBlocked {
	pub request_id: RequestId,
}

impl Message for RequestsBlocked {
	const ID: u64 = 0x1a;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}
