use object_store::path::{Path, PathPart};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::{Error, ID_MAX, Result};

/// Bytes that are not `A-Z a-z 0-9 _ -` and must be percent-encoded in a track name.
const TRACK: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'-');

/// Percent-encode a track name as one object-store path segment.
pub(crate) fn encode_track(name: &str) -> Result<String> {
	if name.is_empty() {
		return Err(Error::Track);
	}
	Ok(utf8_percent_encode(name, TRACK).to_string())
}

/// Decode a percent-encoded track name, refusing anything that is not canonical.
pub(crate) fn decode_track(encoded: &str) -> Result<String> {
	if encoded.is_empty() || encoded.starts_with('.') || encoded.contains('/') {
		return Err(Error::Track);
	}
	let decoded = percent_decode_str(encoded).decode_utf8().map_err(|_| Error::Track)?;
	if encode_track(&decoded)? != encoded {
		return Err(Error::Track);
	}
	Ok(decoded.into_owned())
}

/// Refuse an ID outside 0 through 2^53 - 1.
pub(crate) fn check_id(id: u64) -> Result<u64> {
	if id > ID_MAX { Err(Error::Id(id)) } else { Ok(id) }
}

pub(crate) fn push(base: &Path, segment: &str) -> Result<Path> {
	let part = PathPart::parse(segment).map_err(|err| Error::Path(err.to_string()))?;
	Ok(base.clone().join(part))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn catalog_json_is_percent_encoded() {
		assert_eq!(encode_track("catalog.json").unwrap(), "catalog%2Ejson");
	}

	#[test]
	fn unreserved_bytes_stay_literal() {
		assert_eq!(encode_track("AZaz09_-").unwrap(), "AZaz09_-");
	}

	#[test]
	fn slash_and_dot_prefix_are_encoded() {
		assert_eq!(encode_track(".hidden").unwrap(), "%2Ehidden");
		assert_eq!(encode_track("a/b").unwrap(), "a%2Fb");
	}

	#[test]
	fn hex_is_uppercase() {
		assert_eq!(encode_track(" ").unwrap(), "%20");
		assert_eq!(encode_track("é").unwrap(), "%C3%A9");
	}

	#[test]
	fn empty_track_is_rejected() {
		assert!(matches!(encode_track(""), Err(Error::Track)));
		assert!(matches!(decode_track(""), Err(Error::Track)));
	}

	#[test]
	fn decode_requires_canonical_encoding() {
		assert_eq!(decode_track("catalog%2Ejson").unwrap(), "catalog.json");
		assert!(matches!(decode_track("catalog%2ejson"), Err(Error::Track)));
		assert!(matches!(decode_track("%41"), Err(Error::Track)));
		assert!(matches!(decode_track("a/b"), Err(Error::Track)));
		assert!(matches!(decode_track(".info"), Err(Error::Track)));
	}
}
