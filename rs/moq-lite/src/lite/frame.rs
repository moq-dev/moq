use crate::{
	coding::{Decode, DecodeError, Encode},
	lite::{Message, Version},
	Time,
};

#[derive(Clone, Debug)]
pub struct Frame {
	pub timestamp: Time,
	pub size: usize,
}

impl Message for Frame {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let timestamp = match version {
			// For backwards compatibility, we encode None as MAX.
			Version::Draft03 => Time::decode(r, version)?,
			// If no timestamp is provided for this protocol version, we use the current (receive) time.
			// NOTE: The (correct) media timestamp is still in the payload for backwards compatibility.
			Version::Draft02 | Version::Draft01 => tokio::time::Instant::now().into(),
		};

		Ok(Self {
			timestamp,
			size: usize::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft03 => self.timestamp.encode(w, version),
			Version::Draft02 | Version::Draft01 => {}
		}

		self.size.encode(w, version);
	}
}
