use std::borrow::Cow;

use crate::{
	coding::{Decode, DecodeError, Encode},
	ietf::{Message, MessageParameters, Version},
};

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

impl RequestId {
	pub fn increment(&mut self) -> RequestId {
		let prev = self.0;
		self.0 += 2;
		RequestId(prev)
	}
}

impl std::fmt::Display for RequestId {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		write!(f, "{}", self.0)
	}
}

impl<V> Encode<V> for RequestId {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
		self.0.encode(w, version);
	}
}

impl<V> Decode<V> for RequestId {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let request_id = u64::decode(r, version)?;
		Ok(Self(request_id))
	}
}

#[derive(Clone, Debug)]
pub struct MaxRequestId {
	pub request_id: RequestId,
}

impl Message for MaxRequestId {
	const ID: u64 = 0x15;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

#[derive(Clone, Debug)]
pub struct RequestsBlocked {
	pub request_id: RequestId,
}

impl Message for RequestsBlocked {
	const ID: u64 = 0x1a;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

/// REQUEST_OK (0x07 in v15) - Generic success response for any request.
/// Replaces PublishNamespaceOk, SubscribeNamespaceOk in v15.
/// Also used as response to SubscribeUpdate and TrackStatus in v15.
#[derive(Clone, Debug)]
pub struct RequestOk {
	pub request_id: RequestId,
	pub parameters: MessageParameters,
}

impl Message for RequestOk {
	const ID: u64 = 0x07;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.parameters.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let parameters = MessageParameters::decode(r, version)?;
		Ok(Self { request_id, parameters })
	}
}

/// REQUEST_ERROR (0x05 in v15) - Generic error response for any request.
/// Replaces SubscribeError, PublishError, PublishNamespaceError,
/// SubscribeNamespaceError, FetchError in v15.
#[derive(Clone, Debug)]
pub struct RequestError<'a> {
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for RequestError<'_> {
	const ID: u64 = 0x05;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.request_id.encode(w, version);
		self.error_code.encode(w, version);
		self.reason_phrase.encode(w, version);
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let error_code = u64::decode(r, version)?;
		let reason_phrase = Cow::<str>::decode(r, version)?;
		Ok(Self {
			request_id,
			error_code,
			reason_phrase,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version);
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, version)
	}

	#[test]
	fn test_request_ok_round_trip() {
		let msg = RequestOk {
			request_id: RequestId(42),
			parameters: MessageParameters::default(),
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: RequestOk = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(42));
	}

	#[test]
	fn test_request_error_round_trip() {
		let msg = RequestError {
			request_id: RequestId(99),
			error_code: 500,
			reason_phrase: "Internal error".into(),
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: RequestError = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, RequestId(99));
		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Internal error");
	}
}
