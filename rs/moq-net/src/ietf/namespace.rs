use crate::{Path, coding::*};

use super::Version;

fn to_tuple(namespace: &Path) -> Vec<String> {
	let path = namespace.as_str();
	if path.is_empty() {
		return Vec::new();
	}

	let mut parts = Vec::new();
	let mut part = String::new();
	let mut chars = path.chars();
	while let Some(ch) = chars.next() {
		match ch {
			'/' => parts.push(std::mem::take(&mut part)),
			'\\' => match chars.next() {
				Some(next @ ('/' | '\\')) => part.push(next),
				Some(next) => {
					part.push('\\');
					part.push(next);
				}
				None => part.push('\\'),
			},
			_ => part.push(ch),
		}
	}
	parts.push(part);

	parts
}

fn from_tuple(parts: &[String]) -> Path<'static> {
	let mut path = String::new();
	for (i, part) in parts.iter().enumerate() {
		if i > 0 {
			path.push('/');
		}

		for ch in part.chars() {
			if matches!(ch, '/' | '\\') {
				path.push('\\');
			}
			path.push(ch);
		}
	}

	Path::from_escaped(path)
}

/// Helper function to encode namespace as tuple of strings
pub fn encode_namespace<W: bytes::BufMut>(w: &mut W, namespace: &Path, version: Version) -> Result<(), EncodeError> {
	let parts = to_tuple(namespace);

	// The IETF draft limits namespaces to 32 parts.
	if parts.len() > Path::MAX_PARTS {
		return Err(BoundsExceeded.into());
	}

	(parts.len() as u64).encode(w, version)?;
	for part in parts {
		part.encode(w, version)?;
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
	if count > Path::MAX_PARTS as u64 {
		return Err(DecodeError::BoundsExceeded);
	}

	let count = count as usize;
	let mut parts = Vec::with_capacity(count);
	for _ in 0..count {
		let part = String::decode(r, version)?;
		parts.push(part);
	}

	Ok(from_tuple(&parts))
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_ns(path: &str) -> Vec<u8> {
		encode_path(&Path::from(path.to_string()))
	}

	fn encode_path(path: &Path<'_>) -> Vec<u8> {
		let mut buf = BytesMut::new();
		encode_namespace(&mut buf, path, Version::Draft17).unwrap();
		buf.to_vec()
	}

	fn decode_ns(bytes: &[u8]) -> Path<'static> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		decode_namespace(&mut buf, Version::Draft17).unwrap()
	}

	fn encode_tuple(parts: &[&str]) -> Vec<u8> {
		let mut buf = BytesMut::new();
		(parts.len() as u64).encode(&mut buf, Version::Draft17).unwrap();
		for part in parts {
			(*part).encode(&mut buf, Version::Draft17).unwrap();
		}
		buf.to_vec()
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

	#[test]
	fn slash_in_tuple_part_is_escaped() {
		let tuple = encode_tuple(&["foo/bar", "baz"]);
		let decoded = decode_ns(&tuple);
		assert_eq!(decoded.as_str(), r"foo\/bar/baz");
		assert_eq!(encode_path(&decoded), tuple);
	}

	#[test]
	fn literal_backslash_round_trips() {
		let tuple = encode_tuple(&[r"foo\bar/baz", "qux"]);
		let decoded = decode_ns(&tuple);
		assert_eq!(decoded.as_str(), r"foo\\bar\/baz/qux");
		assert_eq!(encode_path(&decoded), tuple);
	}

	#[test]
	fn slashes_at_part_boundaries_round_trip() {
		let tuple = encode_tuple(&["/foo", "bar/", "/baz/"]);
		let decoded = decode_ns(&tuple);
		assert_eq!(decoded.as_str(), r"\/foo/bar\//\/baz\/");
		assert_eq!(encode_path(&decoded), tuple);
	}
}
