use crate::{Path, coding::*, path::TooLong};

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

fn from_tuple(parts: &[&str]) -> Result<Path, TooLong> {
	// Sized for the common case of no escapes, so building it rarely grows.
	let mut path = String::with_capacity(parts.iter().map(|part| part.len() + 1).sum());
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
pub fn encode_namespace(w: &mut Encoder<'_>, namespace: &Path) -> Result<(), EncodeError> {
	let parts = to_tuple(namespace);

	// The IETF draft limits namespaces to 32 parts.
	if parts.len() > Path::MAX_PARTS {
		return Err(BoundsExceeded.into());
	}

	w.varint(parts.len() as u64)?;
	for part in parts {
		w.string(&part)?;
	}
	Ok(())
}

/// The most bytes a Track Namespace, or a whole Full Track Name, may hold on the wire.
const MAX_BYTES: usize = 4096;

/// Decode a namespace tuple, along with the sum of its field lengths.
///
/// Every limit here closes the session as a PROTOCOL_VIOLATION (draft-22 section 8.7):
/// more than 32 fields, an empty field, or more than 4096 bytes.
fn decode_fields(r: &mut Decoder<'_>) -> Result<(Path, usize), DecodeError> {
	let count = r.varint()?;

	if count == 0 {
		return Ok((Path::empty(), 0));
	}

	if count > Path::MAX_PARTS as u64 {
		return Err(DecodeError::BoundsExceeded);
	}

	// Borrow each field from the wire, so only the joined path allocates.
	let mut parts = [""; Path::MAX_PARTS];
	let parts = &mut parts[..count as usize];
	let mut size = 0;
	for part in parts.iter_mut() {
		*part = r.str()?;
		if part.is_empty() {
			return Err(DecodeError::InvalidValue);
		}
		size += part.len();
		if size > MAX_BYTES {
			return Err(DecodeError::BoundsExceeded);
		}
	}

	Ok((from_tuple(parts)?, size))
}

/// Decode a Track Namespace.
pub fn decode_namespace(r: &mut Decoder<'_>) -> Result<Path, DecodeError> {
	Ok(decode_fields(r)?.0)
}

/// Decode a Full Track Name: a Track Namespace, then a Track Name, at most 4096 bytes together.
pub fn decode_track(r: &mut Decoder<'_>) -> Result<(Path, String), DecodeError> {
	let (namespace, size) = decode_fields(r)?;
	let name = r.string()?;
	if size + name.len() > MAX_BYTES {
		return Err(DecodeError::BoundsExceeded);
	}
	Ok((namespace, name))
}

/// Session namespaces belong to the transport, and none of our extensions defines one.
pub(super) fn refusal(namespace: &Path, version: super::Version) -> Option<super::RequestError<'static>> {
	if matches!(version, super::Version::Draft18) {
		return None;
	}
	// Reserved literals contain no escapes. Comparing the first escaped field keeps
	// a slash within a field and an empty first field from becoming a reserved name.
	if !matches!(namespace.as_str().split('/').next(), Some("." | ".session")) {
		return None;
	}
	Some(super::RequestError {
		error_code: super::error::request::to_code(
			&crate::Error::NotFound,
			super::error::request::Kind::Subscribe,
			version,
		),
		reason_phrase: "reserved namespace does not exist".into(),
		retry_interval: 0,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reserved_namespaces_keep_exact_tuple_fields_and_draft_boundaries() {
		for (fields, reserved) in [
			(vec!["."], true),
			(vec![".", "child"], true),
			(vec![".session"], true),
			(vec![".session", "extension"], true),
			(vec![".session/extension"], false),
			(vec!["", ".session"], false),
			(vec![".application"], false),
			(vec![".sessionish"], false),
		] {
			let path = from_tuple(&fields).unwrap();
			assert_eq!(
				refusal(&path, super::super::Version::Draft22).is_some(),
				reserved,
				"{fields:?}"
			);
			assert!(refusal(&path, super::super::Version::Draft18).is_none());
		}
	}
	use crate::coding::Form;

	const FORM: Form = Form::LeadingOnes;

	fn encode_ns(path: &str) -> Vec<u8> {
		encode_path(&crate::Path::try_from(path.to_string()).unwrap())
	}

	fn encode_path(path: &Path) -> Vec<u8> {
		let mut buf = Vec::new();
		encode_namespace(&mut Encoder::new(&mut buf, FORM), path).unwrap();
		buf
	}

	fn decode_ns(bytes: &[u8]) -> Path {
		decode_namespace(&mut Decoder::new(bytes, FORM)).unwrap()
	}

	fn encode_tuple(parts: &[&str]) -> Vec<u8> {
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, FORM);
		w.varint(parts.len() as u64).unwrap();
		for part in parts {
			w.string(part).unwrap();
		}
		buf
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

	/// Draft-22 section 8.7: every field holds a byte, and a namespace or a whole Full Track
	/// Name holds at most 4096.
	#[test]
	fn names_are_bounded() {
		let decode_track_slice = |bytes: &[u8]| decode_track(&mut Decoder::new(bytes, FORM));

		assert!(decode_namespace(&mut Decoder::new(&encode_tuple(&["a", "", "b"]), FORM)).is_err());

		let field = "n".repeat(4096);
		assert!(decode_namespace(&mut Decoder::new(&encode_tuple(&[&field]), FORM)).is_ok());
		let field = "n".repeat(4097);
		assert!(decode_namespace(&mut Decoder::new(&encode_tuple(&[&field]), FORM)).is_err());

		for (namespace, name, ok) in [(4095, 1, true), (4096, 1, false), (2048, 2049, false)] {
			let mut bytes = encode_tuple(&[&"n".repeat(namespace)]);
			Encoder::new(&mut bytes, FORM).string(&"x".repeat(name)).unwrap();
			assert_eq!(decode_track_slice(&bytes).is_ok(), ok, "{namespace} + {name}");
		}
	}
}
