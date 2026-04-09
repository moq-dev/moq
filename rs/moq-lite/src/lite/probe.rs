use crate::coding::*;

use super::{Message, Version};

/// Sent to probe the available bitrate and round-trip time.
///
/// Lite03+. Lite04 adds the `rtt` field.
#[derive(Clone, Debug)]
pub struct Probe {
	pub bitrate: u64,
	pub rtt: u64,
}

impl Message for Probe {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let bitrate = u64::decode(r, version)?;
		let rtt = match version {
			Version::Lite03 => 0,
			_ => u64::decode(r, version)?,
		};

		Ok(Self { bitrate, rtt })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.bitrate.encode(w, version)?;
		match version {
			Version::Lite03 => {}
			_ => {
				self.rtt.encode(w, version)?;
			}
		}
		Ok(())
	}
}
