use bytes::Buf;

use crate::{
	coding::{Decode, DecodeError, Encode},
	lite::Version,
	Time,
};

#[derive(Clone, Debug)]
pub struct FrameHeader {
	pub timestamp: Time,

	// NOTE: This is the size of the payload that still needs to be read/written.
	// We do not encode/decode here so we can perform chunking.
	pub size: usize,
}

// NOTE: We do not implement Message so we can perform chunking.
// The caller is expected to read/write `size` bytes immediately afterwards.
impl Decode<Version> for FrameHeader {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		// NOTE: This is the message size.
		let size = usize::decode(r, version)?;
		if size > r.remaining() {
			return Err(DecodeError::Short);
		}

		let r = &mut r.take(size);

		let timestamp = match version {
			Version::Draft03 => Time::decode(r, version)?,
			// If no timestamp is provided for this protocol version, we use the current (receive) time.
			// NOTE: The (correct) media timestamp is still in the payload for backwards compatibility.
			Version::Draft02 | Version::Draft01 => tokio::time::Instant::now().into(),
		};

		let size = r.remaining();

		Ok(Self { timestamp, size })
	}
}

impl Encode<Version> for FrameHeader {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		// Unfortunately, we need to know the size of the header to calculate the message size.
		// Encode the timestamp to the maximum buffer size first.
		let mut tmp = [0u8; 8];
		let mut buf = &mut tmp[..];

		match version {
			Version::Draft03 => self.timestamp.encode(&mut buf, version),
			Version::Draft02 | Version::Draft01 => {}
		}

		// Compute the number of bytes used for the timestamp and write it.
		let size = 8 - buf.len();

		// Encode the total size of the timestamp and payload.
		(self.size + size).encode(w, version);

		w.put(&tmp[..size]);
	}
}
