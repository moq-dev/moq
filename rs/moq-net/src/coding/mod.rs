//! Contains encoding and decoding helpers.

mod codes;
mod decode;
mod encode;
#[cfg(feature = "async")]
mod reader;
#[cfg(feature = "async")]
mod stream;
pub mod varint;
mod version;
#[cfg(feature = "async")]
mod writer;

pub use codes::*;
pub use decode::*;
pub use encode::*;
#[cfg(feature = "async")]
pub use reader::*;
#[cfg(feature = "async")]
pub use stream::*;
pub use varint::BoundsExceeded;
pub(crate) use varint::Form;
pub use version::*;
#[cfg(feature = "async")]
pub use writer::*;

/// Decode from the front of a test buffer with `decode`, advancing it past what was read.
#[cfg(test)]
pub(crate) fn decode_buf<B: bytes::Buf, V: Into<Form> + Copy, T>(
	buf: &mut B,
	version: V,
	decode: impl FnOnce(&mut Decoder<'_>, V) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
	let chunk = buf.chunk();
	let mut r = Decoder::new(chunk, version.into());
	let value = decode(&mut r, version)?;
	let used = chunk.len() - r.remaining();
	buf.advance(used);
	Ok(value)
}

/// [`decode_buf`] from a slice, so the value may borrow from it.
#[cfg(test)]
pub(crate) fn decode_from<'a, V: Into<Form> + Copy, T>(
	buf: &mut &'a [u8],
	version: V,
	decode: impl FnOnce(&mut Decoder<'a>, V) -> Result<T, DecodeError>,
) -> Result<T, DecodeError> {
	let mut r = Decoder::new(buf, version.into());
	let value = decode(&mut r, version)?;
	*buf = &buf[buf.len() - r.remaining()..];
	Ok(value)
}

/// Decode one varint from the front of a test buffer, advancing it past what was read.
#[cfg(test)]
#[cfg(feature = "async")]
pub(crate) fn decode_varint<B: bytes::Buf, V: Into<Form> + Copy>(buf: &mut B, version: V) -> Result<u64, DecodeError> {
	decode_buf(buf, version, |r, _| r.varint())
}
