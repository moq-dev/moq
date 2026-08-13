use std::borrow::Cow;

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::Message;

use super::Version;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(pub u64);

impl RequestId {
	/// Returns the previous request ID and advances by 2.
	///
	/// IDs increment by 2 so peers keep parity separation:
	/// clients use even IDs and servers use odd IDs.
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

impl Encode<Version> for RequestId {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.0.encode(w, version)?;
		Ok(())
	}
}

impl Decode<Version> for RequestId {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
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

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
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

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

/// REQUEST_OK (0x07 in v15) - Generic success response for any request.
/// Replaces PublishNamespaceOk, SubscribeNamespaceOk in v15.
/// Also used as response to SubscribeUpdate and TrackStatus in v15.
#[derive(Clone, Debug, Default)]
pub struct RequestOk {
	pub request_id: Option<RequestId>,

	/// MoQ Namespace Count: how many NAMESPACE messages make up the initial set, on a
	/// response to SUBSCRIBE_NAMESPACE. Only sent to a peer that asked for it, since an
	/// unknown parameter closes the session, and ignored on any other response.
	pub namespace_count: Option<u64>,
}

impl Message for RequestOk {
	const ID: u64 = 0x07;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			self.request_id
				.expect("request_id required for draft14-16")
				.encode(w, version)?;
		} else {
			assert!(self.request_id.is_none(), "request_id must be None for draft17+");
		}
		encode_params!(w, version,
			super::namespace_count::NAMESPACE_COUNT_PARAM => self.namespace_count,
		);
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			Some(RequestId::decode(r, version)?)
		} else {
			None
		};
		decode_params!(r, version,
			super::namespace_count::NAMESPACE_COUNT_PARAM => namespace_count: Option<u64>,
		);
		Ok(Self {
			request_id,
			namespace_count,
		})
	}
}

/// REQUEST_ERROR (0x05 in v15) - Generic error response for any request.
/// Replaces SubscribeError, PublishError, PublishNamespaceError,
/// SubscribeNamespaceError, FetchError in v15.
#[derive(Clone, Debug)]
pub struct RequestError<'a> {
	pub request_id: Option<RequestId>,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
	/// v16+: retry interval in milliseconds
	pub retry_interval: u64,
}

impl Message for RequestError<'_> {
	const ID: u64 = 0x05;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			self.request_id
				.expect("request_id required for draft14-16")
				.encode(w, version)?;
		} else {
			assert!(self.request_id.is_none(), "request_id must be None for draft17+");
		}
		self.error_code.encode(w, version)?;
		if !matches!(version, Version::Draft14 | Version::Draft15) {
			self.retry_interval.encode(w, version)?;
		}
		self.reason_phrase.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			Some(RequestId::decode(r, version)?)
		} else {
			None
		};
		let error_code = u64::decode(r, version)?;
		let retry_interval = match version {
			Version::Draft14 | Version::Draft15 => 0,
			_ => u64::decode(r, version)?,
		};
		let reason_phrase = Cow::<str>::decode(r, version)?;
		Ok(Self {
			request_id,
			error_code,
			reason_phrase,
			retry_interval,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn encode_message<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version).unwrap();
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, version)
	}

	#[test]
	fn test_request_ok_round_trip() {
		let msg = RequestOk {
			request_id: Some(RequestId(42)),
			..Default::default()
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: RequestOk = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(42)));
	}

	/// The MoQ Namespace Count parameter rides the SUBSCRIBE_NAMESPACE response, so a
	/// count of 0 (an empty initial set) must survive the round trip as `Some(0)`: it is
	/// the answer "this prefix has nothing", not the absence of an answer.
	#[test]
	fn namespace_count_round_trips() {
		for version in [Version::Draft16, Version::Draft17, Version::Draft18, Version::Draft19] {
			for count in [0, 1, 1000] {
				let msg = RequestOk {
					request_id: matches!(version, Version::Draft16).then_some(RequestId(2)),
					namespace_count: Some(count),
				};

				let encoded = encode_message(&msg, version);
				let decoded: RequestOk = decode_message(&encoded, version).unwrap();

				assert_eq!(decoded.namespace_count, Some(count), "version {version}");
			}
		}
	}

	/// A peer that never heard of the extension sends no parameter, which must stay
	/// distinguishable from a count of 0.
	#[test]
	fn namespace_count_absent_is_none() {
		let encoded = encode_message(&RequestOk::default(), Version::Draft19);
		let decoded: RequestOk = decode_message(&encoded, Version::Draft19).unwrap();

		assert_eq!(decoded.namespace_count, None);
	}

	#[test]
	fn test_request_error_round_trip() {
		let msg = RequestError {
			request_id: Some(RequestId(99)),
			error_code: 500,
			reason_phrase: "Internal error".into(),
			retry_interval: 0,
		};

		let encoded = encode_message(&msg, Version::Draft15);
		let decoded: RequestError = decode_message(&encoded, Version::Draft15).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(99)));
		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Internal error");
		assert_eq!(decoded.retry_interval, 0);
	}

	#[test]
	fn test_request_error_v16_retry_interval() {
		let msg = RequestError {
			request_id: Some(RequestId(99)),
			error_code: 500,
			reason_phrase: "Internal error".into(),
			retry_interval: 5000,
		};

		let encoded = encode_message(&msg, Version::Draft16);
		let decoded: RequestError = decode_message(&encoded, Version::Draft16).unwrap();

		assert_eq!(decoded.request_id, Some(RequestId(99)));
		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Internal error");
		assert_eq!(decoded.retry_interval, 5000);
	}

	#[test]
	fn test_request_ok_v17_round_trip() {
		let msg = RequestOk::default();

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: RequestOk = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, None);
	}

	#[test]
	fn test_request_error_v17_round_trip() {
		let msg = RequestError {
			request_id: None,
			error_code: 500,
			reason_phrase: "Internal error".into(),
			retry_interval: 3000,
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: RequestError = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, None);
		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Internal error");
		assert_eq!(decoded.retry_interval, 3000);
	}

	#[test]
	fn test_request_ok_v18_round_trip() {
		let msg = RequestOk::default();

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: RequestOk = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, None);
	}

	/// Regression: pre-fix, the `version != Draft17` check caused Draft18 to be
	/// treated as Draft14-16 and panic in the encoder.
	#[test]
	fn test_request_ok_v18_wire_matches_v17() {
		let msg = RequestOk::default();
		let v17 = encode_message(&msg, Version::Draft17);
		let v18 = encode_message(&msg, Version::Draft18);
		assert_eq!(v17, v18);
	}

	#[test]
	fn test_request_error_v18_round_trip() {
		let msg = RequestError {
			request_id: None,
			error_code: 500,
			reason_phrase: "Internal error".into(),
			retry_interval: 3000,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: RequestError = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, None);
		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Internal error");
		assert_eq!(decoded.retry_interval, 3000);
	}
}
