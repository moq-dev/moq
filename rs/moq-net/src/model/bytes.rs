use bytes::{Bytes, BytesMut};

/// Converts borrowed or owned byte buffers into [`Bytes`].
///
/// Owned buffers keep their allocation when possible. Borrowed buffers copy into
/// a new [`Bytes`] value.
pub trait AsBytes: AsRef<[u8]> {
	/// Convert this buffer into owned bytes.
	fn into_bytes(self) -> Bytes;
}

impl AsBytes for Bytes {
	fn into_bytes(self) -> Bytes {
		self
	}
}

impl AsBytes for &Bytes {
	fn into_bytes(self) -> Bytes {
		self.clone()
	}
}

impl AsBytes for BytesMut {
	fn into_bytes(self) -> Bytes {
		self.freeze()
	}
}

impl AsBytes for &BytesMut {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self.as_ref())
	}
}

impl AsBytes for Vec<u8> {
	fn into_bytes(self) -> Bytes {
		Bytes::from(self)
	}
}

impl AsBytes for &Vec<u8> {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self)
	}
}

impl AsBytes for String {
	fn into_bytes(self) -> Bytes {
		Bytes::from(self)
	}
}

impl AsBytes for &String {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self.as_bytes())
	}
}

impl AsBytes for &str {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self.as_bytes())
	}
}

impl AsBytes for &[u8] {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self)
	}
}

impl<const N: usize> AsBytes for &[u8; N] {
	fn into_bytes(self) -> Bytes {
		Bytes::copy_from_slice(self)
	}
}
