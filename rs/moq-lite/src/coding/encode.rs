use std::{borrow::Cow, sync::Arc};

use bytes::{Bytes, BytesMut};

/// Write the value to the buffer using the given version.
pub trait Encode<V>: Sized {
	/// Encode the value to the given writer.
	///
	/// This will panic if the [bytes::BufMut] does not have enough capacity.
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V);

	/// Encode the value into a [Bytes] buffer.
	///
	/// NOTE: This will allocate.
	fn encode_bytes(&self, v: V) -> Bytes {
		let mut buf = BytesMut::new();
		self.encode(&mut buf, v);
		buf.freeze()
	}
}

impl<V> Encode<V> for bool {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) {
		w.put_u8(*self as u8);
	}
}

impl<V> Encode<V> for u8 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) {
		w.put_u8(*self);
	}
}

impl<V> Encode<V> for u16 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) {
		w.put_u16(*self);
	}
}

impl<V> Encode<V> for String {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.as_str().encode(w, version)
	}
}

impl<V> Encode<V> for &str {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.len().encode(w, version);
		w.put(self.as_bytes());
	}
}

impl<V> Encode<V> for i8 {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, _: V) {
		// This is not the usual way of encoding negative numbers.
		// i8 doesn't exist in the draft, but we use it instead of u8 for priority.
		// A default of 0 is more ergonomic for the user than a default of 128.
		w.put_u8(((*self as i16) + 128) as u8);
	}
}

impl<T: Encode<V>, V: Clone> Encode<V> for &[T] {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.len().encode(w, version.clone());
		for item in self.iter() {
			item.encode(w, version.clone());
		}
	}
}

impl<V> Encode<V> for Vec<u8> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.len().encode(w, version);
		w.put_slice(self);
	}
}

impl<V> Encode<V> for bytes::Bytes {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.len().encode(w, version);
		w.put_slice(self);
	}
}

impl<T: Encode<V>, V> Encode<V> for Arc<T> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		(**self).encode(w, version);
	}
}

impl<V> Encode<V> for Cow<'_, str> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.len().encode(w, version);
		w.put(self.as_bytes());
	}
}

impl<V> Encode<V> for Option<u64> {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		match self {
			Some(value) => (value + 1).encode(w, version),
			None => 0u64.encode(w, version),
		}
	}
}

impl<V> Encode<V> for std::time::Duration {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		// TODO Make encoding fallable.
		(self.as_millis() as u64).encode(w, version)
	}
}
