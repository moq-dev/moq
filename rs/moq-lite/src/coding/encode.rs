use std::{borrow::Cow, sync::Arc};

use bytes::{Bytes, BytesMut};

use super::BoundsExceeded;

/// An error that occurs during encoding.
#[derive(thiserror::Error, Debug, Clone)]
#[non_exhaustive]
pub enum EncodeError {
	#[error("bounds exceeded")]
	BoundsExceeded(#[from] BoundsExceeded),
	#[error("too large")]
	TooLarge,
	#[error("short buffer")]
	Short,
	#[error("invalid state")]
	InvalidState,
	#[error("too many")]
	TooMany,
	#[error("unsupported version")]
	Version,
}

/// Check that the writer has enough remaining capacity.
fn check_remaining(w: &impl bytes::BufMut, needed: usize) -> Result<(), EncodeError> {
	if w.remaining_mut() < needed {
		return Err(EncodeError::Short);
	}
	Ok(())
}

/// Write the value to the buffer using the given version.
pub trait Encode<V>: Sized {
	/// Encode the value to the given writer.
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError>;

	/// Encode the value into a [Bytes] buffer.
	///
	/// NOTE: This will allocate.
	fn encode_bytes(&self, v: V) -> Result<Bytes, EncodeError> {
		let mut buf = BytesMut::new();
		self.encode(&mut buf, v)?;
		Ok(buf.freeze())
	}
}

impl<V> Encode<V> for bool {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) -> Result<(), EncodeError> {
		check_remaining(&*w, 1)?;
		w.put_u8(*self as u8);
		Ok(())
	}
}

impl<V> Encode<V> for u8 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) -> Result<(), EncodeError> {
		check_remaining(&*w, 1)?;
		w.put_u8(*self);
		Ok(())
	}
}

impl<V> Encode<V> for u16 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) -> Result<(), EncodeError> {
		check_remaining(&*w, 2)?;
		w.put_u16(*self);
		Ok(())
	}
}

impl<V> Encode<V> for String {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.as_str().encode(w, version)
	}
}

impl<V> Encode<V> for &str {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.len().encode(w, version)?;
		check_remaining(&*w, self.len())?;
		w.put(self.as_bytes());
		Ok(())
	}
}

impl<V> Encode<V> for i8 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) -> Result<(), EncodeError> {
		// This is not the usual way of encoding negative numbers.
		// i8 doesn't exist in the draft, but we use it instead of u8 for priority.
		// A default of 0 is more ergonomic for the user than a default of 128.
		check_remaining(&*w, 1)?;
		w.put_u8(((*self as i16) + 128) as u8);
		Ok(())
	}
}

impl<T: Encode<V>, V: Clone> Encode<V> for &[T] {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.len().encode(w, version.clone())?;
		for item in self.iter() {
			item.encode(w, version.clone())?;
		}
		Ok(())
	}
}

impl<V> Encode<V> for Vec<u8> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.len().encode(w, version)?;
		check_remaining(&*w, self.len())?;
		w.put_slice(self);
		Ok(())
	}
}

impl<V> Encode<V> for bytes::Bytes {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.len().encode(w, version)?;
		check_remaining(&*w, self.len())?;
		w.put_slice(self);
		Ok(())
	}
}

impl<T: Encode<V>, V> Encode<V> for Arc<T> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		(**self).encode(w, version)
	}
}

impl<V> Encode<V> for Cow<'_, str> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		self.len().encode(w, version)?;
		check_remaining(&*w, self.len())?;
		w.put(self.as_bytes());
		Ok(())
	}
}

impl<V> Encode<V> for Option<u64> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		match self {
			Some(value) => value.checked_add(1).ok_or(EncodeError::TooLarge)?.encode(w, version),
			None => 0u64.encode(w, version),
		}
	}
}

impl<V> Encode<V> for std::time::Duration {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) -> Result<(), EncodeError> {
		let ms = super::VarInt::try_from(self.as_millis())?;
		ms.encode(w, version)
	}
}
