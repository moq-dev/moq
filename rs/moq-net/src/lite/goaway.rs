use std::borrow::Cow;

use crate::coding::*;

use super::{Message, Version};

/// Sent to gracefully shut down a session and optionally redirect to a new URI.
///
/// Lite04+ only.
#[derive(Clone, Debug)]
pub struct Goaway<'a> {
	pub uri: Cow<'a, str>,
}

impl Message for Goaway<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		// Cap the URI at 8,192 bytes, matching the IETF wire's New Session URI
		// cap. Rejected from the string's length prefix alone, before allocating
		// or validating the payload. (Buffering is bounded separately by the
		// outer message-size prefix that frames every lite control message.)
		let len = usize::decode(r, version)?;
		if len > 8192 {
			return Err(DecodeError::InvalidValue);
		}
		if r.remaining() < len {
			return Err(DecodeError::Short);
		}
		let uri = String::from_utf8(r.copy_to_bytes(len).to_vec())?;
		Ok(Self { uri: Cow::Owned(uri) })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.uri.encode(w, version)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	#[test]
	fn roundtrip_with_uri() {
		let msg = Goaway {
			uri: Cow::Borrowed("https://relay.example/new"),
		};
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Lite04).unwrap();

		let decoded = Goaway::decode_msg(&mut buf.freeze(), Version::Lite04).unwrap();
		assert_eq!(decoded.uri, "https://relay.example/new");
	}

	#[test]
	fn roundtrip_empty() {
		let msg = Goaway { uri: Cow::Borrowed("") };
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Lite04).unwrap();

		let decoded = Goaway::decode_msg(&mut buf.freeze(), Version::Lite04).unwrap();
		assert_eq!(decoded.uri, "");
	}

	#[test]
	fn rejected_before_lite04() {
		let msg = Goaway {
			uri: Cow::Borrowed("https://relay.example/new"),
		};
		let mut buf = BytesMut::new();

		// Encoding should fail on Lite03.
		assert!(msg.encode_msg(&mut buf, Version::Lite03).is_err());

		// Even if we have valid bytes, decoding on Lite03 should fail.
		let mut encode_buf = BytesMut::new();
		msg.encode_msg(&mut encode_buf, Version::Lite04).unwrap();
		assert!(Goaway::decode_msg(&mut encode_buf.freeze(), Version::Lite03).is_err());
	}

	/// The URI is capped at 8,192 bytes (matching the IETF wire), rejected from
	/// the length prefix alone so a hostile length can't force unbounded buffering.
	#[test]
	fn rejects_oversized_uri() {
		// Exactly at the cap: accepted.
		let at_cap = "a".repeat(8192);
		let msg = Goaway {
			uri: Cow::Borrowed(&at_cap),
		};
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Lite04).unwrap();
		let decoded = Goaway::decode_msg(&mut buf.freeze(), Version::Lite04).unwrap();
		assert_eq!(decoded.uri.len(), 8192);

		// One byte over: rejected as InvalidValue, without needing the payload
		// bytes to be present (the length prefix alone is enough to reject).
		let over_cap = "a".repeat(8193);
		let msg = Goaway {
			uri: Cow::Borrowed(&over_cap),
		};
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Lite04).unwrap();
		let mut truncated = buf.freeze();
		// Keep only the length prefix plus a little payload.
		let mut short = truncated.split_to(16);
		assert!(matches!(
			Goaway::decode_msg(&mut short, Version::Lite04),
			Err(DecodeError::InvalidValue)
		));
	}
}
