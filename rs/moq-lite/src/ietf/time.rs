use crate::{
	coding::{Decode, DecodeError, Encode},
	ietf::Version,
	Time,
};

impl Decode<Version> for Time {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let v = u64::decode(r, version)?;
		match version {
			// Milliseconds are used in the IETF draft.
			Version::Draft14 => Ok(Self::from_millis(v).map_err(|_| DecodeError::InvalidValue)?),
		}
	}
}

impl Encode<Version> for Time {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft14 => self.as_millis().encode(w, version),
		}
	}
}
