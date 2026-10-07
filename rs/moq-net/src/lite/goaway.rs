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
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
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
		let len = r.varint()?;
		if len > 8192 {
			return Err(DecodeError::InvalidValue);
		}
		let uri = String::from_utf8(r.slice(len as usize)?.to_vec())?;
		Ok(Self { uri: Cow::Owned(uri) })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		w.string(&self.uri)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn roundtrip_with_uri() {
		let msg = Goaway {
			uri: Cow::Borrowed("https://relay.example/new"),
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
			.unwrap();

		let decoded =
			crate::coding::decode_buf(&mut bytes::Bytes::from(buf), Version::Lite04, Goaway::decode_msg).unwrap();
		assert_eq!(decoded.uri, "https://relay.example/new");
	}

	#[test]
	fn roundtrip_empty() {
		let msg = Goaway { uri: Cow::Borrowed("") };
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
			.unwrap();

		let decoded =
			crate::coding::decode_buf(&mut bytes::Bytes::from(buf), Version::Lite04, Goaway::decode_msg).unwrap();
		assert_eq!(decoded.uri, "");
	}

	#[test]
	fn rejected_before_lite04() {
		let msg = Goaway {
			uri: Cow::Borrowed("https://relay.example/new"),
		};
		let mut buf = Vec::new();

		// Encoding should fail on Lite03.
		assert!(
			msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite03.into()), Version::Lite03)
				.is_err()
		);

		// Even if we have valid bytes, decoding on Lite03 should fail.
		let mut encode_buf = Vec::new();
		msg.encode_msg(
			&mut Encoder::new(&mut encode_buf, Version::Lite04.into()),
			Version::Lite04,
		)
		.unwrap();
		assert!(
			crate::coding::decode_buf(&mut bytes::Bytes::from(encode_buf), Version::Lite03, Goaway::decode_msg)
				.is_err()
		);
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
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
			.unwrap();
		let decoded =
			crate::coding::decode_buf(&mut bytes::Bytes::from(buf), Version::Lite04, Goaway::decode_msg).unwrap();
		assert_eq!(decoded.uri.len(), 8192);

		// One byte over: rejected as InvalidValue, without needing the payload
		// bytes to be present (the length prefix alone is enough to reject).
		let over_cap = "a".repeat(8193);
		let msg = Goaway {
			uri: Cow::Borrowed(&over_cap),
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04)
			.unwrap();
		let mut truncated = bytes::Bytes::from(buf);
		// Keep only the length prefix plus a little payload.
		let mut short = truncated.split_to(16);
		assert!(matches!(
			crate::coding::decode_buf(&mut short, Version::Lite04, Goaway::decode_msg),
			Err(DecodeError::InvalidValue)
		));
	}
}
