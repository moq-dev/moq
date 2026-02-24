use crate::{
	coding::*,
	lite::{Message, Version},
};

/// Sent to probe the available bitrate.
///
/// Draft03 only.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Probe {
	pub bitrate: u64,
}

impl Message for Probe {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let bitrate = u64::decode(r, version)?;

		Ok(Self { bitrate })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.bitrate.encode(w, version);
	}
}
