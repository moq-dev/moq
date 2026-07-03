use bytes::{Bytes, BytesMut};

/// Converts borrowed or owned byte buffers into [`Bytes`].
///
/// Owned buffers keep their allocation when possible. Borrowed buffers copy into
/// a new [`Bytes`] value.
pub trait IntoBytes: AsRef<[u8]> {
	/// Convert this buffer into owned bytes.
	fn into_bytes(self) -> Bytes;
}

impl IntoBytes for Bytes {
	fn into_bytes(self) -> Bytes {
		self
	}
}

impl IntoBytes for &Bytes {
	fn into_bytes(self) -> Bytes {
		self.clone()
	}
}

impl IntoBytes for BytesMut {
	fn into_bytes(self) -> Bytes {
		self.freeze()
	}
}

impl IntoBytes for &BytesMut {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self.as_ref())
	}
}

impl IntoBytes for Vec<u8> {
	fn into_bytes(self) -> Bytes {
		Bytes::from(self)
	}
}

impl IntoBytes for &Vec<u8> {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self)
	}
}

impl IntoBytes for String {
	fn into_bytes(self) -> Bytes {
		Bytes::from(self)
	}
}

impl IntoBytes for &String {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self.as_bytes())
	}
}

impl IntoBytes for &str {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self.as_bytes())
	}
}

impl IntoBytes for &[u8] {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self)
	}
}

impl<const N: usize> IntoBytes for &[u8; N] {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self)
	}
}
