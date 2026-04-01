use crate::{Path, coding::*};

use super::Version;

/// Helper function to encode namespace as tuple of strings
pub fn encode_namespace<W: bytes::BufMut>(w: &mut W, namespace: &Path, version: Version) -> Result<(), EncodeError> {
	// Split the path by '/' to get individual parts
	let path_str = namespace.as_str();
	if path_str.is_empty() {
		0u64.encode(w, version)?;
	} else {
		let parts: Vec<&str> = path_str.split('/').collect();

		// The IETF draft limits namespaces to 32 parts.
		if parts.len() > 32 {
			return Err(BoundsExceeded.into());
		}

		(parts.len() as u64).encode(w, version)?;
		for part in parts {
			part.encode(w, version)?;
		}
	}
	Ok(())
}

/// Helper function to decode namespace from tuple of strings
pub fn decode_namespace<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Path<'static>, DecodeError> {
	let count = u64::decode(r, version)?;

	if count == 0 {
		return Ok(Path::from(String::new()));
	}

	// The IETF draft limits namespaces to 32 parts.
	if count > 32 {
		return Err(DecodeError::BoundsExceeded);
	}

	let count = count as usize;
	let mut parts = Vec::with_capacity(count);
	for _ in 0..count {
		let part = String::decode(r, version)?;
		parts.push(part);
	}

	Ok(Path::from(parts.join("/")))
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_ns(path: &str) -> Vec<u8> {
		let mut buf = BytesMut::new();
		encode_namespace(&mut buf, &Path::from(path.to_string()), Version::Draft17).unwrap();
		buf.to_vec()
	}

	fn decode_ns(bytes: &[u8]) -> Path<'static> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		decode_namespace(&mut buf, Version::Draft17).unwrap()
	}

	#[test]
	fn empty_encodes_as_zero_length_tuple() {
		let bytes = encode_ns("");
		// Should be a single byte: varint 0 (zero parts)
		assert_eq!(bytes, vec![0x00]);
	}

	#[test]
	fn empty_round_trip() {
		let bytes = encode_ns("");
		let decoded = decode_ns(&bytes);
		assert_eq!(decoded.as_str(), "");
	}

	#[test]
	fn single_part_round_trip() {
		let bytes = encode_ns("test");
		let decoded = decode_ns(&bytes);
		assert_eq!(decoded.as_str(), "test");
	}

	#[test]
	fn single_part_encodes_count_one() {
		let bytes = encode_ns("test");
		assert_eq!(bytes[0], 0x01);
	}

	#[test]
	fn multi_part_round_trip() {
		let bytes = encode_ns("conference/room/123");
		let decoded = decode_ns(&bytes);
		assert_eq!(decoded.as_str(), "conference/room/123");
	}

	#[test]
	fn multi_part_encodes_correct_count() {
		let bytes = encode_ns("a/b/c");
		assert_eq!(bytes[0], 0x03);
	}
}
