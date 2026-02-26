use crate::{
	coding::*,
	lite::{Message, Version},
};

#[derive(Clone, Debug)]
pub struct SessionInfo {
	pub bitrate: Option<u64>,
}

impl Message for SessionInfo {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft01 | Version::Draft02 => {}
			Version::Draft03 => return Err(DecodeError::Version),
		}

		let bitrate = match u64::decode(r, version)? {
			0 => None,
			bitrate => Some(bitrate),
		};

		Ok(Self { bitrate })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft01 | Version::Draft02 => {}
			Version::Draft03 => return Err(EncodeError::Version),
		}

		self.bitrate.unwrap_or(0).encode(w, version)?;
		Ok(())
	}
}
