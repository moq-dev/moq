//! IETF moq-transport-14 goaway message

use std::borrow::Cow;

use crate::{
	coding::*,
	ietf::{Message, Version},
};

/// GoAway message (0x10)
#[derive(Clone, Debug)]
pub struct GoAway<'a> {
	pub new_session_uri: Cow<'a, str>,
}

impl Message for GoAway<'_> {
	const ID: u64 = 0x10;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.new_session_uri.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let new_session_uri = Cow::<str>::decode(r, version)?;
		Ok(Self { new_session_uri })
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Draft14);
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8]) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, Version::Draft14)
	}

	#[test]
	fn test_goaway_with_url() {
		let msg = GoAway {
			new_session_uri: "https://example.com/new".into(),
		};

		let encoded = encode_message(&msg);
		let decoded: GoAway = decode_message(&encoded).unwrap();

		assert_eq!(decoded.new_session_uri, "https://example.com/new");
	}

	#[test]
	fn test_goaway_empty() {
		let msg = GoAway {
			new_session_uri: "".into(),
		};

		let encoded = encode_message(&msg);
		let decoded: GoAway = decode_message(&encoded).unwrap();

		assert_eq!(decoded.new_session_uri, "");
	}
}
