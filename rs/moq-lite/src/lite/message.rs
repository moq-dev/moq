use bytes::{Buf, BufMut};

use crate::{
	coding::{Decode, DecodeError, Encode, EncodeError, Sizer},
	lite::Version,
};

/// A trait for messages that are automatically size-prefixed during encoding/decoding.
///
/// This trait wraps the existing Encode/Decode traits and automatically handles:
/// - Prefixing messages with their encoded size during encoding
/// - Reading the size prefix and validating exact consumption during decoding
/// - Ensuring no bytes are left over or missing after decoding
pub trait Message: Sized {
	/// Encode this message with a size prefix.
	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError>;

	/// Decode a size-prefixed message, ensuring exact size consumption.
	fn decode_msg<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError>;
}

// Blanket implementation for all types that implement Encode + Decode
impl<T: Message> Encode<Version> for T {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let mut sizer = Sizer::default();
		Message::encode_msg(self, &mut sizer, version)?;
		sizer.size.encode(w, version)?;
		Message::encode_msg(self, w, version)
	}
}

impl<T: Message> Decode<Version> for T {
	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let size = usize::decode(buf, version)?;
		let mut limited = buf.take(size);
		let result = Message::decode_msg(&mut limited, version)?;
		if limited.remaining() > 0 {
			return Err(DecodeError::Long);
		}

		Ok(result)
	}
}
