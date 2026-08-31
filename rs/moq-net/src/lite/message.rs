use bytes::{Buf, BufMut};

use crate::coding::{Decode, DecodeError, Encode, EncodeError, Sizer};

use super::Version;

// Match the JavaScript reader's ceiling. Lite control messages are buffered before
// decoding, so the limit must be checked as soon as their length prefix arrives.
pub(super) const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

pub(super) fn decode_size<B: Buf>(buf: &mut B, version: Version) -> Result<usize, DecodeError> {
	let size = usize::decode(buf, version)?;
	if size > MAX_MESSAGE_SIZE {
		return Err(DecodeError::MessageTooLarge {
			size,
			max: MAX_MESSAGE_SIZE,
		});
	}
	Ok(size)
}

/// A trait for lite messages that are automatically size-prefixed during encoding/decoding.
///
/// Lite messages use a varint size prefix.
pub trait Message: Sized + std::fmt::Debug {
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
		sizer.size.encode(w, version)?;
		self.encode_msg(w, version)
	}
}

impl<T: Message> Decode<Version> for T {
	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		let size = decode_size(buf, version)?;

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
			if buf.remaining() < size {
				return Err(DecodeError::Short);
			}
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

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Debug)]
	struct Empty;

	impl Message for Empty {
		fn encode_msg<W: BufMut>(&self, _: &mut W, _: Version) -> Result<(), EncodeError> {
			Ok(())
		}

		fn decode_msg<B: Buf>(_: &mut B, _: Version) -> Result<Self, DecodeError> {
			Ok(Self)
		}
	}

	#[test]
	fn rejects_oversized_message_before_reading_the_body() {
		let mut wire = Vec::new();
		((MAX_MESSAGE_SIZE + 1) as u64)
			.encode(&mut wire, Version::Lite06Wip)
			.unwrap();

		let err = Empty::decode(&mut wire.as_slice(), Version::Lite06Wip).unwrap_err();
		assert!(matches!(
			err,
			DecodeError::MessageTooLarge {
				size,
				max: MAX_MESSAGE_SIZE,
			} if size == MAX_MESSAGE_SIZE + 1
		));
	}

	#[test]
	fn accepts_message_at_the_limit() {
		let mut wire = Vec::new();
		(MAX_MESSAGE_SIZE as u64).encode(&mut wire, Version::Lite06Wip).unwrap();

		let err = Empty::decode(&mut wire.as_slice(), Version::Lite06Wip).unwrap_err();
		assert!(matches!(err, DecodeError::Short));
	}
}
