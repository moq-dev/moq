//! The MoQ Pattern extension (draft-lcurley-moq-pattern).
//!
//! Negotiated independently of clustering. Pattern-only sessions carry no cluster
//! metadata; when both extensions are on, NAMESPACE uses one shared parameter block.

use bytes::{Buf, BufMut};

use crate::{
	Pattern,
	coding::{Decode, DecodeError, Encode, EncodeError},
	path::Segment,
};

use super::{Param, Parameters, Version};

/// NAMESPACE_PATTERNS Setup Option. Even, so the value is a bare varint. Only 1 enables it.
pub const NAMESPACE_PATTERNS: u64 = 0x40B5C;

/// NAMESPACE_PATTERN message parameter. Odd, so the value is length-prefixed kinds.
pub const NAMESPACE_PATTERN: u64 = 0x40B59;

const KIND_LITERAL: u64 = 0;
const KIND_WILDCARD: u64 = 1;
const KIND_GLOBSTAR: u64 = 2;
const KIND_PARTIAL: u64 = 3;

/// Whether a version negotiates this extension.
pub fn supported(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16)
}

/// What the peer declared for this extension.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peer {
	/// Whether both endpoints sent value 1.
	pub negotiated: bool,
}

/// Read the Setup Option out of a decoded SETUP parameter block.
pub fn peer_from_setup(params: &Parameters, version: Version) -> Result<Peer, DecodeError> {
	if !supported(version) {
		return Ok(Peer::default());
	}
	Ok(Peer {
		negotiated: params.get_varint(super::ParameterVarInt::NamespacePatterns) == Some(1),
	})
}

/// Write our Setup Option into a SETUP parameter block.
pub fn peer_into_setup(params: &mut Parameters, version: Version) {
	if !supported(version) {
		return;
	}
	params.set_varint(super::ParameterVarInt::NamespacePatterns, 1);
}

/// Encode a pattern's segment kinds as the NAMESPACE_PATTERN parameter value.
pub fn encode_kinds(pattern: &Pattern, version: Version) -> Result<Vec<u8>, EncodeError> {
	let mut buf = Vec::new();
	for segment in pattern.segments() {
		kind_of(segment).encode(&mut buf, version)?;
	}
	Ok(buf)
}

/// Decode kinds, then the matching tuple fields, into a pattern.
///
/// `None` means an unknown kind was present: retain the identity, ignore the advertisement.
pub fn decode_pattern(kinds: &[u8], fields: &[String], version: Version) -> Result<Option<Pattern>, DecodeError> {
	let mut buf = bytes::Bytes::from(kinds.to_vec());
	let mut parsed = Vec::new();
	let mut ignored = false;
	while buf.has_remaining() {
		parsed.push(u64::decode(&mut buf, version)?);
	}
	if parsed.len() != fields.len() {
		return Err(DecodeError::InvalidValue);
	}
	let mut segments = Vec::with_capacity(parsed.len());
	for (kind, field) in parsed.into_iter().zip(fields) {
		match decode_field(kind, field, version)? {
			Some(segment) => {
				if !ignored {
					segments.push(segment);
				}
			}
			None => ignored = true,
		}
	}
	if ignored {
		return Ok(None);
	}
	Pattern::new(segments).map(Some).map_err(|_| DecodeError::InvalidValue)
}

/// Encode one pattern segment as a tuple field.
#[allow(dead_code)]
pub fn encode_field<W: BufMut>(w: &mut W, version: Version, segment: &Segment) -> Result<(), EncodeError> {
	match segment {
		Segment::Literal(literal) => literal.encode(w, version),
		Segment::Wildcard | Segment::Globstar => "".encode(w, version),
		Segment::Partial { prefix, suffix } => {
			let mut value = Vec::new();
			(prefix.len() as u64).encode(&mut value, version)?;
			value.extend_from_slice(prefix.as_bytes());
			value.extend_from_slice(suffix.as_bytes());
			value.encode(w, version)
		}
		_ => Err(EncodeError::Unsupported),
	}
}

fn kind_of(segment: &Segment) -> u64 {
	match segment {
		Segment::Literal(_) => KIND_LITERAL,
		Segment::Wildcard => KIND_WILDCARD,
		Segment::Globstar => KIND_GLOBSTAR,
		Segment::Partial { .. } => KIND_PARTIAL,
		_ => KIND_LITERAL,
	}
}

fn decode_field(kind: u64, field: &str, version: Version) -> Result<Option<Segment>, DecodeError> {
	match kind {
		KIND_LITERAL => {
			if field.is_empty() || field.contains(['/', '*']) {
				return Err(DecodeError::InvalidValue);
			}
			Ok(Some(Segment::Literal(field.to_string())))
		}
		KIND_WILDCARD => {
			if !field.is_empty() {
				return Err(DecodeError::InvalidValue);
			}
			Ok(Some(Segment::Wildcard))
		}
		KIND_GLOBSTAR => {
			if !field.is_empty() {
				return Err(DecodeError::InvalidValue);
			}
			Ok(Some(Segment::Globstar))
		}
		KIND_PARTIAL => {
			let mut buf = bytes::Bytes::from(field.as_bytes().to_vec());
			let prefix_len = usize::decode(&mut buf, version)?;
			if buf.remaining() < prefix_len {
				return Err(DecodeError::InvalidValue);
			}
			let prefix_bytes = buf.copy_to_bytes(prefix_len);
			let suffix_bytes = buf.copy_to_bytes(buf.remaining());
			let prefix = std::str::from_utf8(&prefix_bytes).map_err(|_| DecodeError::InvalidValue)?;
			let suffix = std::str::from_utf8(&suffix_bytes).map_err(|_| DecodeError::InvalidValue)?;
			if (prefix.is_empty() && suffix.is_empty()) || prefix.contains(['/', '*']) || suffix.contains(['/', '*']) {
				return Err(DecodeError::InvalidValue);
			}
			Ok(Some(Segment::Partial {
				prefix: prefix.to_string(),
				suffix: suffix.to_string(),
			}))
		}
		_ => Ok(None),
	}
}

/// The NAMESPACE_PATTERN parameter value: one kind varint per tuple field.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Kinds(pub Vec<u8>);

impl Param for Kinds {
	fn param_encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.0.encode(w, version)
	}

	fn param_decode<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Ok(Self(Vec::<u8>::decode(r, version)?))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn kinds_round_trip_a_wildcard() {
		let pattern: Pattern = "live/*".parse().unwrap();
		let kinds = encode_kinds(&pattern, Version::Draft19).unwrap();
		let fields = vec!["live".to_string(), String::new()];
		let decoded = decode_pattern(&kinds, &fields, Version::Draft19).unwrap();
		assert_eq!(decoded.unwrap().as_str(), "live/*");
	}

	#[test]
	fn unknown_kind_is_ignored() {
		let mut kinds = Vec::new();
		99u64.encode(&mut kinds, Version::Draft19).unwrap();
		let decoded = decode_pattern(&kinds, &[String::new()], Version::Draft19).unwrap();
		assert!(decoded.is_none());
	}

	#[test]
	fn setup_option_is_off_until_both_send_one() {
		let mut params = super::Parameters::default();
		assert!(!peer_from_setup(&params, Version::Draft19).unwrap().negotiated);
		peer_into_setup(&mut params, Version::Draft19);
		assert!(peer_from_setup(&params, Version::Draft19).unwrap().negotiated);
		assert!(!peer_from_setup(&params, Version::Draft16).unwrap().negotiated);
	}
}
