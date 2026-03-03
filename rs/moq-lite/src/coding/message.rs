use bytes::{Buf, BufMut};

use super::{Decode, DecodeError, Encode, EncodeError, Sizer};
use crate::Version;

/// A trait for messages that are automatically size-prefixed during encoding/decoding.
///
/// The size prefix format depends on the version:
/// - Lite versions use varint size prefix
/// - IETF versions use u16 size prefix
pub trait Message: Sized + std::fmt::Debug {
	/// Encode this message body (without size prefix).
	fn encode_msg<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError>;
}

/// IETF messages have a message type ID used for control stream dispatch.
pub trait IetfMessage: Message {
	const ID: u64;
}

impl<T: Message> Encode<Version> for T {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		let mut sizer = Sizer::default();
		self.encode_msg(&mut sizer, version)?;

		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {
				sizer.size.encode(w, version)?;
			}
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				let size: u16 = sizer.size.try_into().map_err(|_| EncodeError::TooLarge)?;
				size.encode(w, version)?;
			}
		}

		self.encode_msg(w, version)
	}
}

impl<T: Message> Decode<Version> for T {
	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let size = match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => usize::decode(buf, version)?,
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17 => {
				u16::decode(buf, version)? as usize
			}
		};

		let mut limited = buf.take(size);
		let result = Self::decode_msg(&mut limited, version)?;
		if limited.remaining() > 0 {
			return Err(DecodeError::Long);
		}

		Ok(result)
	}
}
