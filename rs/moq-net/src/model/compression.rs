//! Per-frame payload compression.
//!
//! A publisher marks a [`crate::Track`] with `compress = true` when its frames are
//! worth compressing (e.g. a JSON catalog). The wire protocol then negotiates a
//! concrete [`Compression`] codec in SUBSCRIBE_OK, and every frame on that track is
//! compressed independently so the codec doesn't carry state across the lossy,
//! out-of-order group boundary.

use std::io::{Read, Write};

use crate::{Error, MAX_FRAME_SIZE, Result};

/// The codec used to (de)compress frame payloads, negotiated per subscription.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Compression {
	/// Frames are sent verbatim.
	#[default]
	None,
	/// Raw DEFLATE (RFC 1951), no zlib/gzip header. QUIC already guarantees
	/// integrity, so the extra checksum bytes of zlib/gzip would be wasted.
	Deflate,
}

impl Compression {
	/// Compress a whole frame payload.
	///
	/// [`Compression::None`] returns the input unchanged. The caller decides
	/// whether the result is actually smaller; this just applies the codec.
	pub fn compress(&self, data: &[u8]) -> Vec<u8> {
		match self {
			Self::None => data.to_vec(),
			Self::Deflate => {
				let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
				// Writing into a Vec is infallible.
				encoder.write_all(data).expect("deflate write to vec");
				encoder.finish().expect("deflate finish to vec")
			}
		}
	}

	/// Decompress a whole frame payload, rejecting anything that inflates past
	/// `MAX_FRAME_SIZE` so a malicious peer can't zip-bomb the receiver.
	pub fn decompress(&self, data: &[u8]) -> Result<Vec<u8>> {
		match self {
			Self::None => Ok(data.to_vec()),
			Self::Deflate => {
				// Read one byte past the limit so we can tell "exactly at the cap"
				// apart from "overflowed".
				let mut decoder = flate2::read::DeflateDecoder::new(data).take(MAX_FRAME_SIZE + 1);
				let mut out = Vec::new();
				decoder.read_to_end(&mut out).map_err(|_| Error::Decompress)?;
				if out.len() as u64 > MAX_FRAME_SIZE {
					return Err(Error::FrameTooLarge);
				}
				Ok(out)
			}
		}
	}

	/// The varint code used on the wire.
	pub fn to_code(self) -> u64 {
		match self {
			Self::None => 0,
			Self::Deflate => 1,
		}
	}

	/// Parse a wire varint code, erroring on unknown codecs.
	pub fn from_code(code: u64) -> Result<Self> {
		match code {
			0 => Ok(Self::None),
			1 => Ok(Self::Deflate),
			_ => Err(Error::Unsupported),
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn none_roundtrip() {
		let data = b"the quick brown fox";
		let c = Compression::None;
		let packed = c.compress(data);
		assert_eq!(&packed, data);
		assert_eq!(c.decompress(&packed).unwrap(), data);
	}

	#[test]
	fn deflate_roundtrip() {
		// Highly compressible input so we can assert the codec actually shrinks it.
		let data = vec![b'a'; 4096];
		let c = Compression::Deflate;
		let packed = c.compress(&data);
		assert!(packed.len() < data.len(), "deflate should shrink repetitive data");
		assert_eq!(c.decompress(&packed).unwrap(), data);
	}

	#[test]
	fn deflate_empty() {
		let c = Compression::Deflate;
		let packed = c.compress(&[]);
		assert_eq!(c.decompress(&packed).unwrap(), Vec::<u8>::new());
	}

	#[test]
	fn decompress_rejects_garbage() {
		let c = Compression::Deflate;
		assert!(matches!(c.decompress(b"not a deflate stream"), Err(Error::Decompress)));
	}

	#[test]
	fn code_roundtrip() {
		for c in [Compression::None, Compression::Deflate] {
			assert_eq!(Compression::from_code(c.to_code()).unwrap(), c);
		}
		assert!(Compression::from_code(99).is_err());
	}
}
