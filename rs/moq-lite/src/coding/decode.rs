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
	#[error("short buffer")]
	Short,

	#[error("long buffer")]
	Long,

	#[error("invalid string")]
	InvalidString(#[from] FromUtf8Error),

	#[error("invalid message: {0:?}")]
	InvalidMessage(u64),

	#[error("invalid subscribe location")]
	InvalidSubscribeLocation,

	#[error("invalid value")]
	InvalidValue,

	#[error("too many")]
	TooMany,

	#[error("bounds exceeded")]
	BoundsExceeded,

	#[error("expected end")]
	ExpectedEnd,

	#[error("expected data")]
	ExpectedData,

	#[error("duplicate")]
	Duplicate,

	#[error("missing")]
	Missing,

	#[error("unsupported")]
	Unsupported,
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

impl<V> Decode<V> for String {
	/// Decode a string with a varint length prefix.
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let v = Vec::<u8>::decode(r, version)?;
		let str = String::from_utf8(v)?;

		Ok(str)
	}
}

impl<V> Decode<V> for Vec<u8> {
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

impl<V> Decode<V> for bytes::Bytes {
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
impl<V> Decode<V> for Cow<'_, str> {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let s = String::decode(r, version)?;
		Ok(Cow::Owned(s))
	}
}

impl<V> Decode<V> for Option<u64> {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		match u64::decode(r, version)? {
			0 => Ok(None),
			value => Ok(Some(value - 1)),
		}
	}
}

impl<V> Decode<V> for std::time::Duration {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let value = u64::decode(r, version)?;
		Ok(Self::from_millis(value))
	}
}
