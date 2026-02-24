use crate::coding::*;

use num_enum::{IntoPrimitive, TryFromPrimitive};

#[derive(Debug, PartialEq, Clone, Copy, IntoPrimitive, TryFromPrimitive)]
#[repr(u64)]
pub enum ControlType {
	Session = 0,
	Announce = 1,
	Subscribe = 2,
	Fetch = 3,
	Probe = 4,
}

impl<V> Decode<V> for ControlType {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let t = u64::decode(r, version)?;
		t.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl<V> Encode<V> for ControlType {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		let v: u64 = (*self).into();
		v.encode(w, version)
	}
}

#[derive(Debug, PartialEq, Clone, Copy, IntoPrimitive, TryFromPrimitive)]
#[repr(u64)]
pub enum DataType {
	Group = 0,
}

impl<V> Decode<V> for DataType {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let t = u64::decode(r, version)?;
		t.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl<V> Encode<V> for DataType {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		let v: u64 = (*self).into();
		v.encode(w, version)
	}
}
