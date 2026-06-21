use crate::coding::*;

use super::Version;

use num_enum::{IntoPrimitive, TryFromPrimitive};

#[derive(Debug, PartialEq, Clone, Copy, IntoPrimitive, TryFromPrimitive)]
#[repr(u64)]
pub enum ControlType {
	Session = 0,
	Announce = 1,
	Subscribe = 2,
	Fetch = 3,
	Probe = 4,
	Goaway = 5,
	Track = 6,
}

impl Decode<Version> for ControlType {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let t = u64::decode(r, version)?;
		t.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for ControlType {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let v: u64 = (*self).into();
		v.encode(w, version)?;
		Ok(())
	}
}

#[derive(Debug, PartialEq, Clone, Copy, IntoPrimitive, TryFromPrimitive)]
#[repr(u64)]
pub enum DataType {
	/// A group of frames (the only data stream on every version).
	Group = 0,
	/// The lite-05+ SETUP stream: one SETUP message, then FIN.
	Setup = 1,
}

impl Decode<Version> for DataType {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let t = u64::decode(r, version)?;
		t.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for DataType {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let v: u64 = (*self).into();
		v.encode(w, version)?;
		Ok(())
	}
}
