use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::Version;

/// A trait for IETF messages that are automatically size-prefixed during encoding/decoding.
///
/// IETF messages use a u16 size prefix and have a message type ID for control stream dispatch.
pub trait Message: Sized + std::fmt::Debug {
	const ID: u64;

	/// Encode this message body (without size prefix).
	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError>;

	/// Decode a message body (without size prefix).
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError>;
}

impl<T: Message> Encode<Version> for T {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		tracing::trace!(?self, "encoding");
		let prefix = w.prefix_u16();
		self.encode_msg(w, version)?;
		w.fill(prefix)
	}
}

/// A control message body not decoded yet: a `u16` length, then that many bytes.
///
/// Read after the message type, when the type decides how to decode the rest.
#[derive(Debug)]
pub struct Body(pub bytes::Bytes);

impl Decode<Version> for Body {
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let size = r.u16()? as usize;
		Ok(Self(bytes::Bytes::copy_from_slice(r.slice(size)?)))
	}
}

impl Body {
	/// A decoder over the body, with `version`'s varints.
	pub fn decoder(&self, version: Version) -> Decoder<'_> {
		Decoder::new(&self.0, version.into())
	}
}

impl<T: Message> Decode<Version> for T {
	fn decode(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let size = r.u16()? as usize;
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
