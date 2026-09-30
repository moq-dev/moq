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
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let t = r.varint()?;
		t.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for ControlType {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		let v: u64 = (*self).into();
		w.varint(v)?;
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
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let t = r.varint()?;
		t.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for DataType {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		let v: u64 = (*self).into();
		w.varint(v)?;
		Ok(())
	}
}
