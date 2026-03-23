use bytes::{Buf, BufMut};

use crate::coding::{Decode, DecodeError, Encode, EncodeError, Sizer};

use super::Version;

/// A trait for IETF messages that are automatically size-prefixed during encoding/decoding.
///
/// IETF messages use a u16 size prefix and have a message type ID for control stream dispatch.
pub trait Message: Sized + std::fmt::Debug {
	const ID: u64;

	/// Encode this message body (without size prefix).
	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError>;
}

impl<T: Message> Encode<Version> for T {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding");
		let mut sizer = Sizer::default();
		self.encode_msg(&mut sizer, version)?;
		let size: u16 = sizer.size.try_into().map_err(|_| EncodeError::TooLarge)?;
		size.encode(w, version)?;
		self.encode_msg(w, version)
	}
}

impl<T: Message> Decode<Version> for T {
	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let size = u16::decode(buf, version)? as usize;

		if tracing::enabled!(tracing::Level::TRACE) {
			if buf.remaining() < size {
				return Err(DecodeError::Short);
			}
			let raw = buf.copy_to_bytes(size);
			let mut slice = &raw[..];
			match Self::decode_msg(&mut slice, version) {
				Ok(result) => {
					if slice.remaining() > 0 {
						return Err(DecodeError::Long);
					}
					tracing::trace!(?result, "decoded");
					Ok(result)
				}
				Err(e) => {
					tracing::warn!(%e, ?raw, "decode failed");
					Err(e)
				}
			}
		} else {
			let mut limited = buf.take(size);
			match Self::decode_msg(&mut limited, version) {
				Ok(result) => {
					if limited.remaining() > 0 {
						return Err(DecodeError::Long);
					}
					Ok(result)
				}
				Err(e) => {
					tracing::warn!(%e, "decode failed");
					Err(e)
				}
			}
		}
	}
}
