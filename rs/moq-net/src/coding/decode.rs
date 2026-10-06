use std::string::FromUtf8Error;
use thiserror::Error;

use super::{BoundsExceeded, Form, varint};

/// Read the value from a [`Decoder`] using the given version.
///
/// If [DecodeError::Short] is returned, the caller should try again with more data.
pub trait Decode<V>: Sized {
	/// Decode the value from the front of the decoder.
	fn decode(r: &mut Decoder<'_>, version: V) -> Result<Self, DecodeError>;

	/// Decode the value from the front of `buf`, returning it and the bytes it took.
	fn decode_slice(buf: &[u8], version: V) -> Result<(Self, usize), DecodeError>
	where
		V: Into<Form> + Copy,
	{
		let mut r = Decoder::new(buf, version.into());
		let value = Self::decode(&mut r, version)?;
		Ok((value, buf.len() - r.remaining()))
	}
}

/// A decode error.
#[derive(Error, Debug, Clone)]
#[non_exhaustive]
pub enum DecodeError {
	/// The buffer ran out mid-value. Retry once more bytes arrive.
	#[error("short buffer")]
	Short,

	/// The value claims more bytes than the enclosing message allows.
	#[error("long buffer")]
	Long,

	/// A string field was not valid UTF-8.
	#[error("invalid string")]
	InvalidString(#[from] FromUtf8Error),

	/// The message type ID is unknown for the negotiated version.
	#[error("invalid message: {0:?}")]
	InvalidMessage(u64),

	/// A SUBSCRIBE start/end location is malformed or out of order.
	#[error("invalid subscribe location")]
	InvalidSubscribeLocation,

	/// A field held a value outside its permitted range.
	#[error("invalid value")]
	InvalidValue,

	/// A repeated field exceeded the count this implementation accepts.
	#[error("too many")]
	TooMany,

	/// An integer was too large for the field it was read into.
	#[error("bounds exceeded")]
	BoundsExceeded,

	/// More data followed where the message was required to end.
	#[error("expected end")]
	ExpectedEnd,

	/// A length-prefixed message exceeded the receiver's byte limit.
	#[error("message too large: {size} bytes exceeds {max} byte limit")]
	MessageTooLarge {
		/// The byte length declared by the peer.
		size: usize,
		/// The largest message this receiver accepts.
		max: usize,
	},

	/// The stream ended where a payload was required.
	#[error("expected data")]
	ExpectedData,

	/// A parameter or field appeared more than once.
	#[error("duplicate")]
	Duplicate,

	/// A required parameter or field was absent.
	#[error("missing")]
	Missing,

	/// The value is well-formed but this implementation does not handle it.
	#[error("unsupported")]
	Unsupported,

	/// Bytes remained after the value was fully decoded.
	#[error("trailing bytes")]
	TrailingBytes,

	/// The field does not exist in the negotiated protocol version.
	#[error("unsupported version")]
	Version,
}

impl DecodeError {
	/// A complete frame cannot be extended by reading more stream bytes.
	pub(crate) fn complete(self) -> Self {
		match self {
			Self::Short => Self::InvalidValue,
			other => other,
		}
	}
}

impl From<BoundsExceeded> for DecodeError {
	fn from(_: BoundsExceeded) -> Self {
		Self::BoundsExceeded
	}
}

/// Reads wire primitives from the front of a byte slice.
///
/// A read either consumes exactly what it returns or fails and consumes nothing, so a
/// [`DecodeError::Short`] can be retried once more bytes arrive.
#[derive(Debug, Clone)]
pub struct Decoder<'a> {
	buf: &'a [u8],
	form: Form,
}

impl<'a> Decoder<'a> {
	/// Read `buf`, with varints in the given form.
	pub fn new(buf: &'a [u8], form: Form) -> Self {
		Self { buf, form }
	}

	/// The varint form this decoder reads.
	pub fn form(&self) -> Form {
		self.form
	}

	/// The number of unread bytes.
	pub fn remaining(&self) -> usize {
		self.buf.len()
	}

	/// Whether every byte has been read.
	pub fn is_empty(&self) -> bool {
		self.buf.is_empty()
	}

	/// Read `len` raw bytes.
	pub fn slice(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
		let Some((head, rest)) = self.buf.split_at_checked(len) else {
			return Err(DecodeError::Short);
		};
		self.buf = rest;
		Ok(head)
	}

	/// Read every remaining byte.
	pub fn rest(&mut self) -> &'a [u8] {
		std::mem::take(&mut self.buf)
	}

	/// Split off the next `len` bytes as their own decoder, e.g. a size-prefixed body.
	pub fn sub(&mut self, len: usize) -> Result<Self, DecodeError> {
		Ok(Self::new(self.slice(len)?, self.form))
	}

	/// Read a single byte.
	pub fn u8(&mut self) -> Result<u8, DecodeError> {
		Ok(self.slice(1)?[0])
	}

	/// Read a big-endian `u16`.
	pub fn u16(&mut self) -> Result<u16, DecodeError> {
		let b = self.slice(2)?;
		Ok(u16::from_be_bytes([b[0], b[1]]))
	}

	/// Read a byte that must be 0 or 1.
	pub fn bool(&mut self) -> Result<bool, DecodeError> {
		match self.u8()? {
			0 => Ok(false),
			1 => Ok(true),
			_ => Err(DecodeError::InvalidValue),
		}
	}

	/// Read a varint.
	#[cfg_attr(target_arch = "wasm32", inline)]
	#[cfg_attr(not(target_arch = "wasm32"), inline(always))]
	pub fn varint(&mut self) -> Result<u64, DecodeError> {
		let (value, rest) = varint::read(self.buf, self.form)?;
		self.buf = rest;
		Ok(value)
	}

	/// Read an optional varint: 0 is `None`, and `n + 1` is `Some(n)`.
	pub fn varint_opt(&mut self) -> Result<Option<u64>, DecodeError> {
		Ok(self.varint()?.checked_sub(1))
	}

	/// Read a varint length, then that many raw bytes.
	pub fn bytes(&mut self) -> Result<&'a [u8], DecodeError> {
		let start = self.buf;
		let len = usize::try_from(self.varint()?).map_err(|_| DecodeError::BoundsExceeded)?;
		self.slice(len).inspect_err(|_| self.buf = start)
	}

	/// Read a varint length, then that many bytes of UTF-8.
	pub fn string(&mut self) -> Result<String, DecodeError> {
		Ok(String::from_utf8(self.bytes()?.to_vec())?)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A short read must leave the decoder where it was, or a retry with more bytes
	/// would start mid-value.
	#[test]
	fn short_consumes_nothing() {
		let mut r = Decoder::new(&[0x05, b'a', b'b'], Form::Quic);
		assert!(matches!(r.bytes(), Err(DecodeError::Short)));
		assert_eq!(r.remaining(), 3);

		let mut r = Decoder::new(&[0x40], Form::Quic);
		assert!(matches!(r.varint(), Err(DecodeError::Short)));
		assert_eq!(r.remaining(), 1);
	}
}
