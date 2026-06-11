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

		let uri = Cow::<str>::decode(r, version)?;
		Ok(Self { uri })
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

	fn roundtrip(uri: &str) {
		let msg = Goaway { uri: uri.into() };

		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Lite04).unwrap();

		let mut bytes = bytes::Bytes::from(buf.to_vec());
		let decoded = Goaway::decode_msg(&mut bytes, Version::Lite04).unwrap();

		assert_eq!(decoded.uri, uri);
	}

	#[test]
	fn roundtrip_with_uri() {
		roundtrip("https://example.com/new");
	}

	#[test]
	fn roundtrip_empty() {
		roundtrip("");
	}

	#[test]
	fn rejected_before_lite04() {
		let msg = Goaway {
			uri: "https://example.com/new".into(),
		};
		let mut buf = BytesMut::new();
		assert!(msg.encode_msg(&mut buf, Version::Lite03).is_err());
	}
}
