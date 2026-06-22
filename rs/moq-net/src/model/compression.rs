//! Per-frame payload compression.
//!
//! A publisher marks a [`crate::Track`] with `compress = true` to hint its frames
//! are worth compressing (e.g. a JSON catalog). The wire then negotiates an
//! algorithm per hop (the SETUP `Compression` parameter) and names it per frame, so
//! a frame can opt out (`None`) when compression wouldn't shrink it. Each frame is
//! compressed independently so the codec doesn't carry state across the lossy,
//! out-of-order group boundary.

use std::io::{Read, Write};

use crate::{Error, MAX_FRAME_SIZE, Result};

/// A frame-payload compression codec. "No compression" (verbatim) is the absence
/// of a codec, modeled as `Option::None` rather than a variant here, so the type
/// can't represent a meaningless "compress with nothing" and a negotiated algorithm
/// list can't list it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Compression {
	/// Raw DEFLATE (RFC 1951), no zlib/gzip header. QUIC already guarantees
	/// integrity, so the extra checksum bytes of zlib/gzip would be wasted.
	Deflate,
}

impl Compression {
	/// Compress a whole frame payload with this codec.
	///
	/// The caller decides whether the result is actually smaller; this just applies
	/// the codec. Verbatim transfer is the absence of a codec, so it's handled by the
	/// caller (an `Option<Compression>` of `None`), not here.
	pub fn compress(self, data: &[u8]) -> Vec<u8> {
		match self {
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
	pub fn decompress(self, data: &[u8]) -> Result<Vec<u8>> {
		match self {
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

	/// This codec's wire varint code (always non-zero; verbatim is code `0`, which
	/// has no codec — see [`Self::from_code`]).
	pub fn to_code(self) -> u64 {
		match self {
			Self::Deflate => 1,
		}
	}

	/// Parse a wire varint code into an optional codec: `0` is verbatim (`None`);
	/// other known codes are `Some`. Errors on an unknown codec.
	pub fn from_code(code: u64) -> Result<Option<Self>> {
		match code {
			0 => Ok(None),
			1 => Ok(Some(Self::Deflate)),
			_ => Err(Error::Unsupported),
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

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
		// A codec round-trips through its non-zero code; `0` is verbatim (`None`).
		assert_eq!(
			Compression::from_code(Compression::Deflate.to_code()).unwrap(),
			Some(Compression::Deflate)
		);
		assert_eq!(Compression::from_code(0).unwrap(), None);
		assert!(Compression::from_code(99).is_err());
	}
}
