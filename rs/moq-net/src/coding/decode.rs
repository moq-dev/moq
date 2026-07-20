use std::{borrow::Cow, string::FromUtf8Error};
use thiserror::Error;

/// Read the from the buffer using the given version.
///
/// If [DecodeError::Short] is returned, the caller should try again with more data.
pub trait Decode<V>: Sized {
	/// Decode the value from the given buffer.
	fn decode<B: bytes::Buf>(buf: &mut B, version: V) -> Result<Self, DecodeError>;
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

	/// An integer was too large for the QUIC varint range.
	#[error("bounds exceeded")]
	BoundsExceeded,

	/// More data followed where the message was required to end.
	#[error("expected end")]
	ExpectedEnd,

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

impl<V> Decode<V> for bool {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		match u8::decode(r, version)? {
			0 => Ok(false),
			1 => Ok(true),
			_ => Err(DecodeError::InvalidValue),
		}
	}
}

impl<V> Decode<V> for u8 {
	fn decode<R: bytes::Buf>(r: &mut R, _: V) -> Result<Self, DecodeError> {
		match r.has_remaining() {
			true => Ok(r.get_u8()),
			false => Err(DecodeError::Short),
		}
	}
}

impl<V> Decode<V> for u16 {
	fn decode<R: bytes::Buf>(r: &mut R, _: V) -> Result<Self, DecodeError> {
		match r.remaining() >= 2 {
			true => Ok(r.get_u16()),
			false => Err(DecodeError::Short),
		}
	}
}

impl<V: Copy> Decode<V> for String
where
	usize: Decode<V>,
{
	/// Decode a string with a varint length prefix.
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let v = Vec::<u8>::decode(r, version)?;
		let str = String::from_utf8(v)?;

		Ok(str)
	}
}

impl<V: Copy> Decode<V> for Vec<u8>
where
	usize: Decode<V>,
{
	fn decode<B: bytes::Buf>(buf: &mut B, version: V) -> Result<Self, DecodeError> {
		let size = usize::decode(buf, version)?;

		if buf.remaining() < size {
			return Err(DecodeError::Short);
		}

		let bytes = buf.copy_to_bytes(size);
		Ok(bytes.to_vec())
	}
}

impl<V> Decode<V> for i8 {
	fn decode<R: bytes::Buf>(r: &mut R, _: V) -> Result<Self, DecodeError> {
		if !r.has_remaining() {
			return Err(DecodeError::Short);
		}

		// This is not the usual way of encoding negative numbers.
		// i8 doesn't exist in the draft, but we use it instead of u8 for priority.
		// A default of 0 is more ergonomic for the user than a default of 128.
		Ok(((r.get_u8() as i16) - 128) as i8)
	}
}

impl<V: Copy> Decode<V> for bytes::Bytes
where
	usize: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let len = usize::decode(r, version)?;
		if r.remaining() < len {
			return Err(DecodeError::Short);
		}
		let bytes = r.copy_to_bytes(len);
		Ok(bytes)
	}
}

// TODO Support borrowed strings.
impl<V: Copy> Decode<V> for Cow<'_, str>
where
	usize: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let s = String::decode(r, version)?;
		Ok(Cow::Owned(s))
	}
}

impl<V: Copy> Decode<V> for Option<u64>
where
	u64: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		match u64::decode(r, version)? {
			0 => Ok(None),
			value => Ok(Some(value - 1)),
		}
	}
}

impl<V: Copy> Decode<V> for std::time::Duration
where
	u64: Decode<V>,
{
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let value = u64::decode(r, version)?;
		Ok(Self::from_millis(value))
	}
}
