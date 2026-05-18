//! IETF moq-transport-14 goaway message

use std::borrow::Cow;

use crate::coding::*;

use super::Message;

use super::Version;

/// GoAway message (0x10)
#[derive(Clone, Debug)]
pub struct GoAway<'a> {
	pub new_session_uri: Cow<'a, str>,
	/// Draft-17: timeout in milliseconds before closing the session
	pub timeout: u64,
}

impl Message for GoAway<'_> {
	const ID: u64 = 0x10;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.new_session_uri.encode(w, version)?;
		// Draft-17+ adds a timeout field; draft-18 also adds an optional trailing Request ID
		// (#1559) which we never emit.
		if !matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			self.timeout.encode(w, version)?;
		}
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let new_session_uri = Cow::<str>::decode(r, version)?;
		let timeout = match version {
			Version::Draft14 | Version::Draft15 | Version::Draft16 => 0,
			Version::Draft17 => u64::decode(r, version)?,
			_ => {
				let timeout = u64::decode(r, version)?;
				// Draft-18+: optional trailing Request ID (#1559). Drain if present;
				// moq-lite doesn't act on per-request GOAWAY so the value is discarded.
				if r.has_remaining() {
					let _ = u64::decode(r, version)?;
				}
				timeout
			}
		};
		Ok(Self {
			new_session_uri,
			timeout,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Draft14).unwrap();
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
			timeout: 0,
		};

		let encoded = encode_message(&msg);
		let decoded: GoAway = decode_message(&encoded).unwrap();

		assert_eq!(decoded.new_session_uri, "https://example.com/new");
	}

	#[test]
	fn test_goaway_empty() {
		let msg = GoAway {
			new_session_uri: "".into(),
			timeout: 0,
		};

		let encoded = encode_message(&msg);
		let decoded: GoAway = decode_message(&encoded).unwrap();

		assert_eq!(decoded.new_session_uri, "");
	}

	#[test]
	fn test_goaway_v17_timeout() {
		let msg = GoAway {
			new_session_uri: "https://example.com/new".into(),
			timeout: 5000,
		};

		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Draft17).unwrap();

		let mut bytes = bytes::Bytes::from(buf.to_vec());
		let decoded: GoAway = GoAway::decode_msg(&mut bytes, Version::Draft17).unwrap();

		assert_eq!(decoded.new_session_uri, "https://example.com/new");
		assert_eq!(decoded.timeout, 5000);
	}

	#[test]
	fn test_goaway_v18_timeout() {
		let msg = GoAway {
			new_session_uri: "moqt://relay.example/".into(),
			timeout: 5000,
		};

		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, Version::Draft18).unwrap();

		let mut bytes = bytes::Bytes::from(buf.to_vec());
		let decoded: GoAway = GoAway::decode_msg(&mut bytes, Version::Draft18).unwrap();

		assert_eq!(decoded.new_session_uri, "moqt://relay.example/");
		assert_eq!(decoded.timeout, 5000);
	}

	/// Draft-18 added an optional trailing Request ID (#1559). A peer that emits
	/// one must not break our decoder; we drain and discard it.
	#[test]
	fn test_goaway_v18_drains_optional_request_id() {
		use bytes::Buf;

		// Hand-construct a draft-18 GOAWAY body that includes the optional Request ID.
		let mut buf = BytesMut::new();
		"moqt://relay.example/".encode(&mut buf, Version::Draft18).unwrap();
		5000u64.encode(&mut buf, Version::Draft18).unwrap();
		// Optional trailing Request ID:
		42u64.encode(&mut buf, Version::Draft18).unwrap();

		let mut bytes = bytes::Bytes::from(buf.to_vec());
		let decoded: GoAway = GoAway::decode_msg(&mut bytes, Version::Draft18).unwrap();

		assert_eq!(decoded.new_session_uri, "moqt://relay.example/");
		assert_eq!(decoded.timeout, 5000);
		assert!(!bytes.has_remaining(), "trailing Request ID should be consumed");
	}
}
