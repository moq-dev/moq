use crate::coding::{self, DecodeError, EncodeError, Sizer};
use crate::ietf::Version;
use std::fmt::Debug;

use bytes::{Buf, BufMut};

/// A trait for messages that are size-prefixed during encoding/decoding.
///
/// This trait wraps the existing Encode/Decode traits and automatically handles:
/// - Prefixing messages with their encoded size during encoding
/// - Reading the size prefix and validating exact consumption during decoding
/// - Ensuring no bytes are left over or missing after decoding
pub trait Message: Sized + Debug {
	const ID: u64;

	/// Encode this message with a size prefix.
	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError>;

	/// Decode a size-prefixed message, ensuring exact size consumption.
	fn decode_msg<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError>;
}

impl<T: Message> coding::Encode<Version> for T {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		// TODO Always encode 2 bytes for the size, then go back and populate it later.
		// That way we can avoid calculating the size upfront.
		let mut sizer = Sizer::default();
		self.encode_msg(&mut sizer, version)?;
		let size: u16 = sizer.size.try_into().map_err(|_| EncodeError::TooLarge)?;
		size.encode(w, version)?;
		self.encode_msg(w, version)
	}
}

impl<T: Message> coding::Decode<Version> for T {
	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let size = u16::decode(buf, version)?;
		let mut limited = buf.take(size as usize);
		let result = Self::decode_msg(&mut limited, version)?;
		if limited.remaining() > 0 {
			return Err(DecodeError::Long);
		}
		Ok(result)
	}
}
