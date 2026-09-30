use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::Version;

// Match the JavaScript reader's ceiling. Lite control messages are buffered before
// decoding, so the limit must be checked as soon as their length prefix arrives.
pub(super) const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// Read a lite message's varint size prefix, refusing one past [`MAX_MESSAGE_SIZE`].
pub(super) fn decode_size(r: &mut Decoder<'_>) -> Result<usize, DecodeError> {
	let size = r.varint()?;
	match usize::try_from(size) {
		Ok(size) if size <= MAX_MESSAGE_SIZE => Ok(size),
		_ => Err(DecodeError::MessageTooLarge {
			size: usize::try_from(size).unwrap_or(usize::MAX),
			max: MAX_MESSAGE_SIZE,
		}),
	}
}

/// A trait for lite messages that are automatically size-prefixed during encoding/decoding.
///
/// Lite messages use a varint size prefix.
pub trait Message: Sized + std::fmt::Debug {
	/// Encode this message body (without size prefix).
	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError>;
}

impl<T: Message> Encode<Version> for T {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding");
		let prefix = w.prefix_varint();
		self.encode_msg(w, version)?;
		w.fill(prefix)
	}
}

impl<T: Message> Decode<Version> for T {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let size = decode_size(r)?;
		let mut body = r.sub(size)?;

		let result = Self::decode_msg(&mut body, version).and_then(|msg| match body.is_empty() {
			true => Ok(msg),
			false => Err(DecodeError::Long),
		});

		match &result {
			Ok(msg) => tracing::trace!(?msg, "decoded"),
			Err(err) => tracing::warn!(%err, "decode failed"),
		}
		result
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Debug)]
	struct Empty;

	impl Message for Empty {
		fn encode_msg(&self, _: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
			Ok(())
		}

		fn decode_msg(_: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
			Ok(Self)
		}
	}

	/// A lite size prefix announcing `size` bytes, with no body behind it.
	fn prefix(size: usize) -> Vec<u8> {
		let mut wire = Vec::new();
		Encoder::new(&mut wire, Version::Lite06.into())
			.varint(size as u64)
			.unwrap();
		wire
	}

	#[test]
	fn rejects_oversized_message_before_reading_the_body() {
		let wire = prefix(MAX_MESSAGE_SIZE + 1);

		let err = Empty::decode_slice(&wire, Version::Lite06).unwrap_err();
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
		let wire = prefix(MAX_MESSAGE_SIZE);

		let err = Empty::decode_slice(&wire, Version::Lite06).unwrap_err();
		assert!(matches!(err, DecodeError::Short));
	}
}
