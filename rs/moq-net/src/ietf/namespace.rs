use crate::{Path, coding::*, path::Invalid};

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

// A field ending in `/` escapes to a path with an empty segment, so it is refused, as is a
// namespace whose separators and escapes push its path past `Path::MAX_BYTES`.
fn from_tuple(parts: &[&str]) -> Result<Path, Invalid> {
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

	Path::try_from(path)
}

/// Encode a Track Namespace.
pub fn encode_namespace(w: &mut Encoder<'_>, namespace: &Path) -> Result<(), EncodeError> {
	write_fields(w, &fields(namespace)?)
}

/// Encode a Full Track Name: a Track Namespace, then a Track Name, at most 4096 bytes together.
pub fn encode_track(w: &mut Encoder<'_>, namespace: &Path, name: &str) -> Result<(), EncodeError> {
	let parts = fields(namespace)?;
	if parts.iter().map(String::len).sum::<usize>() + name.len() > MAX_BYTES {
		return Err(BoundsExceeded.into());
	}
	write_fields(w, &parts)?;
	w.string(name)
}

fn fields(namespace: &Path) -> Result<Vec<String>, EncodeError> {
	let parts = to_tuple(namespace);
	// The fields drop the path's separators and escapes, so `Path::MAX_BYTES` already bounds
	// their bytes. A path's depth is unbounded until it reaches a wire.
	if parts.len() > Path::MAX_PARTS {
		return Err(BoundsExceeded.into());
	}
	Ok(parts)
}

fn write_fields(w: &mut Encoder<'_>, parts: &[String]) -> Result<(), EncodeError> {
	w.varint(parts.len() as u64)?;
	for part in parts {
		w.string(part)?;
	}
	Ok(())
}

/// The most bytes a Track Namespace, or a whole Full Track Name, may hold on the wire.
const MAX_BYTES: usize = 4096;

// Every path encodes as a namespace.
const _: () = assert!(Path::MAX_BYTES <= MAX_BYTES);

/// Decode a namespace tuple, along with the sum of its field lengths.
///
/// Every limit here closes the session as a PROTOCOL_VIOLATION (draft-22 section 8.7):
/// more than 32 fields, an empty field, a field ending in `/`, or more than 4096 bytes.
/// So does a namespace too long as a [`Path`], which the draft allows but no path can hold.
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
	// a slash within a field from becoming a reserved name.
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
	fn slashes_at_part_boundaries() {
		// A leading `/` escapes to a non-empty `\` segment.
		let tuple = encode_tuple(&["/foo", "/bar"]);
		let decoded = decode_ns(&tuple);
		assert_eq!(decoded.as_str(), r"\/foo/\/bar");
		assert_eq!(encode_path(&decoded), tuple);

		// A trailing `/` escapes to an empty segment, which every later `Path` operation
		// assumes away, such as stats grouping a path by its leading segments.
		for fields in [&["/foo", "bar/", "/baz/"][..], &["foo/", "bar"], &["/"], &["a//"]] {
			let tuple = encode_tuple(fields);
			let err = decode_namespace(&mut Decoder::new(&tuple, FORM)).unwrap_err();
			assert!(matches!(err, DecodeError::InvalidValue), "{fields:?}: {err:?}");
		}
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

	/// Separators and escapes count toward a path but not its tuple fields, so every path
	/// encodes, while a namespace dense with escaped slashes decodes only if its path fits.
	#[test]
	fn path_bound_covers_namespace_fields() {
		let segments = ["a"; Path::MAX_PARTS - 1].join("/");
		let deepest = format!("{segments}/{}", "é".repeat(Path::MAX_BYTES / 2 - 31));
		let deepest = Path::new(&deepest).unwrap();
		assert_eq!(deepest.len(), Path::MAX_BYTES);
		assert_eq!(decode_ns(&encode_path(&deepest)), deepest);

		let fits = format!("{}a", "/".repeat(Path::MAX_BYTES / 2 - 1));
		assert_eq!(decode_ns(&encode_tuple(&[&fits])).len(), Path::MAX_BYTES - 1);
		let too_long = format!("{}a", "/".repeat(Path::MAX_BYTES / 2));
		let err = decode_namespace(&mut Decoder::new(&encode_tuple(&[&too_long]), FORM)).unwrap_err();
		assert!(matches!(err, DecodeError::BoundsExceeded), "{err:?}");
	}

	/// A Full Track Name counts its namespace's field bytes plus its name, and one too long
	/// is refused before anything is written.
	#[test]
	fn full_track_name_is_bounded_on_encode() {
		// 4095 path bytes holding 2048 field bytes.
		let namespace = Path::new(&format!("{}a", r"\/".repeat(Path::MAX_BYTES / 2 - 1))).unwrap();
		for (len, ok) in [(2048, true), (2049, false)] {
			let name = "x".repeat(len);
			let mut bytes = Vec::new();
			let result = encode_track(&mut Encoder::new(&mut bytes, FORM), &namespace, &name);
			assert_eq!(result.is_ok(), ok, "{len}");
			match ok {
				true => assert_eq!(
					decode_track(&mut Decoder::new(&bytes, FORM)).unwrap(),
					(namespace.clone(), name)
				),
				false => assert!(bytes.is_empty(), "refuse before writing"),
			}
		}
	}
}
