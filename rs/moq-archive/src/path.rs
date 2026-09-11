use object_store::path::{Path, PathPart};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::{Error, ID_MAX, ID_WIDTH, Result};

/// Bytes that are not `A-Z a-z 0-9 _ -` and must be percent-encoded in a track name.
const TRACK: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'-');

/// Percent-encode a track name as one object-store path segment.
pub fn encode_track(name: &str) -> Result<String> {
	if name.is_empty() {
		return Err(Error::Track);
	}
	Ok(utf8_percent_encode(name, TRACK).to_string())
}

/// Decode a percent-encoded track name, refusing anything that is not canonical.
pub fn decode_track(encoded: &str) -> Result<String> {
	if encoded.is_empty() || encoded.starts_with('.') || encoded.contains('/') {
		return Err(Error::Track);
	}
	let decoded = percent_decode_str(encoded).decode_utf8().map_err(|_| Error::Track)?;
	if encode_track(&decoded)? != encoded {
		return Err(Error::Track);
	}
	Ok(decoded.into_owned())
}

/// Write a group or segment ID as 19 zero-padded decimal digits.
pub fn format_id(id: u64) -> Result<String> {
	check_id(id)?;
	Ok(format!("{id:0width$}", width = ID_WIDTH))
}

/// Parse a 19-digit decimal ID field, refusing values outside the recording range.
pub fn parse_id(field: &str) -> Result<u64> {
	if field.len() != ID_WIDTH || !field.bytes().all(|b| b.is_ascii_digit()) {
		return Err(Error::Path(field.to_string()));
	}
	let id: u64 = field.parse().map_err(|_| Error::Path(field.to_string()))?;
	check_id(id)
}

/// Refuse an ID outside 0 through 2^53 - 1.
pub fn check_id(id: u64) -> Result<u64> {
	if id > ID_MAX { Err(Error::Id(id)) } else { Ok(id) }
}

/// A recording object key under an application prefix.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
	/// `<encoded-track>/.info`
	Info {
		/// The unencoded track name.
		track: String,
	},
	/// `<encoded-track>/groups/<largest>.<smallest>`
	Groups {
		/// The unencoded track name.
		track: String,
		/// Inclusive last group sequence in the object.
		largest: u64,
		/// Inclusive first group sequence in the object.
		smallest: u64,
	},
	/// `<encoded-track>/segments/<segment>`
	Segments {
		/// The unencoded track name.
		track: String,
		/// The committed timeline segment ID.
		segment: u64,
	},
}

impl Key {
	/// A track's `.info` object.
	pub fn info(track: impl Into<String>) -> Result<Self> {
		let track = track.into();
		encode_track(&track)?;
		Ok(Self::Info { track })
	}

	/// A range-named groups object. `largest` is the last sequence, `smallest` the first.
	pub fn groups(track: impl Into<String>, largest: u64, smallest: u64) -> Result<Self> {
		let track = track.into();
		encode_track(&track)?;
		check_id(largest)?;
		check_id(smallest)?;
		if largest < smallest {
			return Err(Error::Bounds { smallest, largest });
		}
		Ok(Self::Groups {
			track,
			largest,
			smallest,
		})
	}

	/// A timeline object at `segments/<segment>`.
	pub fn segments(track: impl Into<String>, segment: u64) -> Result<Self> {
		let track = track.into();
		encode_track(&track)?;
		check_id(segment)?;
		Ok(Self::Segments { track, segment })
	}

	/// The unencoded track name this key belongs to.
	pub fn track(&self) -> &str {
		match self {
			Self::Info { track } | Self::Groups { track, .. } | Self::Segments { track, .. } => track,
		}
	}

	/// Encode this key under `prefix`.
	pub fn path(&self, prefix: &Path) -> Result<Path> {
		let path = push(prefix, &encode_track(self.track())?)?;
		match self {
			Self::Info { .. } => push(&path, ".info"),
			Self::Groups { largest, smallest, .. } => {
				let path = push(&path, "groups")?;
				push(&path, &range_name(*largest, *smallest)?)
			}
			Self::Segments { segment, .. } => {
				let path = push(&path, "segments")?;
				push(&path, &format_id(*segment)?)
			}
		}
	}

	/// Parse a store location relative to `prefix`.
	pub fn parse(prefix: &Path, location: &Path) -> Result<Self> {
		if prefix.as_ref().is_empty() {
			return parse_parts(location.parts(), location);
		}
		let parts = location
			.prefix_match(prefix)
			.ok_or_else(|| Error::Path(location.to_string()))?;
		parse_parts(parts, location)
	}
}

fn parse_parts<'a>(mut parts: impl Iterator<Item = PathPart<'a>>, location: &Path) -> Result<Key> {
	let encoded = parts.next().ok_or_else(|| Error::Path(location.to_string()))?;
	let track = decode_track(encoded.as_ref())?;
	let kind = parts.next().ok_or_else(|| Error::Path(location.to_string()))?;
	match kind.as_ref() {
		".info" => {
			if parts.next().is_some() {
				return Err(Error::Path(location.to_string()));
			}
			Ok(Key::Info { track })
		}
		"groups" => {
			let name = parts.next().ok_or_else(|| Error::Path(location.to_string()))?;
			if parts.next().is_some() {
				return Err(Error::Path(location.to_string()));
			}
			let (largest, smallest) = parse_range(name.as_ref())?;
			Ok(Key::Groups {
				track,
				largest,
				smallest,
			})
		}
		"segments" => {
			let name = parts.next().ok_or_else(|| Error::Path(location.to_string()))?;
			if parts.next().is_some() {
				return Err(Error::Path(location.to_string()));
			}
			Ok(Key::Segments {
				track,
				segment: parse_id(name.as_ref())?,
			})
		}
		_ => Err(Error::Path(location.to_string())),
	}
}

/// `<prefix>/<encoded-track>/groups`
pub fn groups_prefix(prefix: &Path, track: &str) -> Result<Path> {
	let path = push(prefix, &encode_track(track)?)?;
	push(&path, "groups")
}

/// `<prefix>/<encoded-track>/segments`
pub fn segments_prefix(prefix: &Path, track: &str) -> Result<Path> {
	let path = push(prefix, &encode_track(track)?)?;
	push(&path, "segments")
}

/// Exclusive listing offset `groups/<group>` (19 digits, no dot) for a FETCH of `group`.
pub fn groups_offset(prefix: &Path, track: &str, group: u64) -> Result<Path> {
	let path = groups_prefix(prefix, track)?;
	push(&path, &format_id(group)?)
}

pub(crate) fn push(base: &Path, segment: &str) -> Result<Path> {
	let part = PathPart::parse(segment).map_err(|err| Error::Path(err.to_string()))?;
	Ok(base.clone().join(part))
}

fn range_name(largest: u64, smallest: u64) -> Result<String> {
	Ok(format!("{}.{}", format_id(largest)?, format_id(smallest)?))
}

fn parse_range(name: &str) -> Result<(u64, u64)> {
	let (largest, smallest) = name.split_once('.').ok_or_else(|| Error::Path(name.to_string()))?;
	if smallest.contains('.') {
		return Err(Error::Path(name.to_string()));
	}
	let largest = parse_id(largest)?;
	let smallest = parse_id(smallest)?;
	if largest < smallest {
		return Err(Error::Bounds { smallest, largest });
	}
	Ok((largest, smallest))
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

	#[test]
	fn id_fields_are_19_digits() {
		assert_eq!(format_id(0).unwrap(), "0000000000000000000");
		assert_eq!(format_id(ID_MAX).unwrap(), "0009007199254740991");
		assert_eq!(parse_id("0000000000000000000").unwrap(), 0);
		assert_eq!(parse_id("0009007199254740991").unwrap(), ID_MAX);
	}

	#[test]
	fn ids_outside_the_json_range_are_rejected() {
		assert!(matches!(format_id(ID_MAX + 1), Err(Error::Id(_))));
		assert!(matches!(check_id(u64::MAX), Err(Error::Id(_))));
		assert!(matches!(
			parse_id("0009007199254740992"),
			Err(Error::Id(n)) if n == ID_MAX + 1
		));
		assert!(matches!(parse_id("5"), Err(Error::Path(_))));
		assert!(matches!(parse_id("000000000000000000X"), Err(Error::Path(_))));
	}

	#[test]
	fn keys_roundtrip_under_a_prefix() {
		let prefix = Path::from("rec/1");
		for key in [
			Key::info("catalog.json").unwrap(),
			Key::groups("video", 10, 5).unwrap(),
			Key::segments("timeline.z", 0).unwrap(),
			Key::segments("timeline.z", ID_MAX).unwrap(),
		] {
			let path = key.path(&prefix).unwrap();
			assert_eq!(Key::parse(&prefix, &path).unwrap(), key);
		}
	}

	#[test]
	fn encoded_track_is_not_double_encoded() {
		let path = Key::info("catalog.json").unwrap().path(&Path::from("rec")).unwrap();
		assert_eq!(path.as_ref(), "rec/catalog%2Ejson/.info");
	}

	#[test]
	fn inverted_range_is_rejected() {
		assert!(matches!(
			Key::groups("v", 1, 2),
			Err(Error::Bounds {
				smallest: 2,
				largest: 1
			})
		));
	}

	#[test]
	fn groups_offset_has_no_dot() {
		let path = groups_offset(&Path::from("rec"), "video", 5).unwrap();
		assert_eq!(path.as_ref(), "rec/video/groups/0000000000000000005");
	}
}
