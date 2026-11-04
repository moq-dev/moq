//! The moq-transport GOAWAY message.

use std::borrow::Cow;

use crate::coding::*;

use super::{Message, RequestId};

use super::Version;

/// GoAway message (0x10)
#[derive(Clone)]
pub struct GoAway<'a> {
	pub new_session_uri: Cow<'a, str>,
	/// Timeout in milliseconds before closing the session.
	pub timeout: u64,
	/// Draft-18 only: the smallest of the receiver's Request IDs the sender has not
	/// processed. Required on the control stream and absent on a request stream.
	pub request_id: Option<RequestId>,
}

impl std::fmt::Debug for GoAway<'_> {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		// The new session URI carries whatever credentials the peer needs to dial it.
		f.debug_struct("GoAway")
			.field("new_session_uri_len", &self.new_session_uri.len())
			.field("timeout", &self.timeout)
			.field("request_id", &self.request_id)
			.finish()
	}
}

impl<'a> Message<'a> for GoAway<'a> {
	const ID: u64 = 0x10;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.string(&self.new_session_uri)?;
		w.varint(self.timeout)?;
		// Draft-18 (#1559) added the Request ID; draft-22 removed it again (#1623).
		if version == Version::Draft18
			&& let Some(request_id) = self.request_id
		{
			request_id.encode(w, version)?;
		}
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let new_session_uri = Cow::Borrowed(r.str()?);
		// All drafts cap the New Session URI at 8,192 bytes; a longer one is a
		// protocol violation.
		if new_session_uri.len() > 8192 {
			return Err(DecodeError::InvalidValue);
		}
		let (timeout, request_id) = match version {
			// The Request ID is present only on the control stream; each caller checks it
			// against the stream it read from.
			Version::Draft18 => {
				let timeout = r.varint()?;
				let request_id = match r.is_empty() {
					true => None,
					false => Some(RequestId::decode(r, version)?),
				};
				(timeout, request_id)
			}
			_ => (r.varint()?, None),
		};
		Ok(Self {
			new_session_uri,
			timeout,
			request_id,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn encode_message<'m, M: Message<'m>>(msg: &M) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Draft22.into()), Version::Draft22)
			.unwrap();
		buf.to_vec()
	}

	fn decode_message<'m, M: Message<'m>>(bytes: &'m [u8]) -> Result<M, DecodeError> {
		let mut buf = bytes;
		crate::coding::decode_from(&mut buf, Version::Draft22, |r, v| M::decode_msg(r, v))
	}

	#[test]
	fn test_goaway_with_url() {
		let msg = GoAway {
			new_session_uri: "https://example.com/new".into(),
			timeout: 0,
			request_id: None,
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
			request_id: None,
		};

		let encoded = encode_message(&msg);
		let decoded: GoAway = decode_message(&encoded).unwrap();

		assert_eq!(decoded.new_session_uri, "");
	}

	#[test]
	fn test_goaway_v18_timeout() {
		let msg = GoAway {
			new_session_uri: "moqt://relay.example/".into(),
			timeout: 5000,
			request_id: Some(RequestId(1)),
		};

		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Draft18.into()), Version::Draft18)
			.unwrap();

		// Draft-18 requires a trailing Request ID on the control stream, so the
		// v18 body must be exactly one varint longer than the v19 body.
		let mut buf19 = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf19, Version::Draft22.into()), Version::Draft22)
			.unwrap();
		assert_eq!(buf.len(), buf19.len() + 1, "v18 must append the Request ID varint");

		let decoded: GoAway = crate::coding::decode_from(&mut &buf[..], Version::Draft18, GoAway::decode_msg).unwrap();

		assert_eq!(decoded.new_session_uri, "moqt://relay.example/");
		assert_eq!(decoded.timeout, 5000);
		assert_eq!(decoded.request_id, Some(RequestId(1)));
	}

	/// Draft-22 (#1623) removed the Request ID again: the body is just URI + timeout,
	/// same bytes we already emit. Round-trip to lock that in.
	#[test]
	fn test_goaway_v19_timeout() {
		let msg = GoAway {
			new_session_uri: "moqt://relay.example/".into(),
			timeout: 5000,
			request_id: None,
		};

		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, Version::Draft18.into()), Version::Draft18)
			.unwrap();

		let decoded: GoAway = crate::coding::decode_from(&mut &buf[..], Version::Draft18, GoAway::decode_msg).unwrap();

		assert_eq!(decoded.new_session_uri, "moqt://relay.example/");
		assert_eq!(decoded.timeout, 5000);
	}

	/// Draft-18 added an optional trailing Request ID (#1559). A peer that emits
	/// one must not break our decoder; we keep it for the parity check.
	#[test]
	fn test_goaway_v18_drains_optional_request_id() {
		// Hand-construct a draft-18 GOAWAY body that includes the optional Request ID.
		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Draft18.into())
			.string("moqt://relay.example/")
			.unwrap();
		Encoder::new(&mut buf, Version::Draft18.into()).varint(5000u64).unwrap();
		// Optional trailing Request ID:
		Encoder::new(&mut buf, Version::Draft18.into()).varint(42u64).unwrap();

		let mut rest = &buf[..];
		let decoded: GoAway = crate::coding::decode_from(&mut rest, Version::Draft18, GoAway::decode_msg).unwrap();

		assert_eq!(decoded.new_session_uri, "moqt://relay.example/");
		assert_eq!(decoded.timeout, 5000);
		assert_eq!(decoded.request_id, Some(RequestId(42)));
		assert!(rest.is_empty(), "trailing Request ID should be consumed");
	}
}
