use crate::coding::*;

use super::{Message, Version};

/// Sent to probe the available bitrate.
///
/// Lite03 only.
#[derive(Clone, Debug)]
pub struct Probe {
	pub bitrate: u64,
}

impl Message for Probe {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
		}

		let bitrate = u64::decode(r, version)?;

		Ok(Self { bitrate })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
		}

		self.bitrate.encode(w, version)?;
		Ok(())
	}
}
