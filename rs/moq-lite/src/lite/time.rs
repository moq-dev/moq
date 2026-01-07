use crate::{
	coding::{Decode, DecodeError, Encode},
	lite::Version,
	Time,
};

impl Decode<Version> for Time {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let v = u64::decode(r, version)?;
		match version {
			// Microseconds are used in the Lite protocol.
			Version::Draft03 => Ok(Self::from_micros(v).map_err(|_| DecodeError::InvalidValue)?),
			// These versions didn't use any timestamps.
			Version::Draft02 | Version::Draft01 => unreachable!(),
		}
	}
}

impl Encode<Version> for Time {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft03 => self.as_micros().encode(w, version),
			// These versions didn't use any timestamps.
			Version::Draft02 | Version::Draft01 => unreachable!(),
		}
	}
}
