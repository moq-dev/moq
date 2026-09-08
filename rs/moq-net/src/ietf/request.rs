use std::borrow::Cow;

use crate::coding::{Decode, DecodeError, Encode, EncodeError};

use super::Message;

use super::{Location, Properties, Version};

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
#[derive(Default, Clone, Debug)]
pub struct RequestOk {
	pub request_id: Option<RequestId>,

	/// The largest Location in the track (LARGEST_OBJECT, 0x09).
	///
	/// Only a TRACK_STATUS_OK carries it, where it is the whole answer; the draft forbids
	/// it in every other REQUEST_OK we send.
	pub largest: Option<Location>,

	/// Metadata about the track, sent as Track Properties (draft-18+).
	///
	/// Only a TRACK_STATUS_OK carries them; the draft requires the block to be empty in
	/// every other REQUEST_OK, and draft-17 has no such field at all.
	pub properties: Properties,
}

impl RequestOk {
	/// Whether this draft puts a Track Properties block at the end of REQUEST_OK.
	///
	/// Draft-18 added it (#1576). Draft-17 has none, and the drafts before it never named
	/// the block outside SUBSCRIBE_OK.
	fn has_properties(version: Version) -> bool {
		!matches!(
			version,
			Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17
		)
	}
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
			0x09 => self.largest,
		);

		// Track Properties are the final field, so nothing may follow.
		if Self::has_properties(version) {
			self.properties.encode(w, version)?;
		}

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = if matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16) {
			Some(RequestId::decode(r, version)?)
		} else {
			None
		};
		decode_params!(r, version,
			0x09 => largest: Option<Location>,
		);
		let properties = match Self::has_properties(version) {
			true => Properties::decode(r, version)?,
			false => Properties::default(),
		};
		Ok(Self {
			request_id,
			largest,
			properties,
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

	/// A TRACK_STATUS_OK's whole answer rides in REQUEST_OK: the LARGEST_OBJECT parameter on
	/// every draft, and the Track Properties block from draft-18 on.
	#[test]
	fn test_request_ok_carries_largest_and_properties() {
		for version in [Version::Draft18, Version::Draft19, Version::Draft20] {
			let msg = RequestOk {
				request_id: None,
				largest: Some(Location { group: 7, object: 1 }),
				properties: Properties {
					timescale: Some(crate::Timescale::MICRO),
					group_order: Some(crate::ietf::GroupOrder::Descending),
				},
			};

			let encoded = encode_message(&msg, version);
			let decoded: RequestOk = decode_message(&encoded, version).unwrap();

			assert_eq!(decoded.largest, Some(Location { group: 7, object: 1 }), "{version}");
			assert_eq!(decoded.properties, msg.properties, "{version}");
		}
	}

	/// Draft-17's REQUEST_OK has no Track Properties field (draft-18 added it), so the block is
	/// dropped rather than written as trailing bytes the peer would fault the message for.
	#[test]
	fn test_request_ok_v17_drops_properties() {
		let msg = RequestOk {
			request_id: None,
			largest: Some(Location { group: 7, object: 1 }),
			properties: Properties {
				timescale: Some(crate::Timescale::MICRO),
				group_order: None,
			},
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: RequestOk = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.largest, Some(Location { group: 7, object: 1 }));
		assert_eq!(decoded.properties, Properties::default());
	}

	/// Draft-15 and draft-16 keep the request id, and LARGEST_OBJECT is the only parameter a
	/// TRACK_STATUS_OK sets there.
	#[test]
	fn test_request_ok_v15_carries_largest() {
		for version in [Version::Draft15, Version::Draft16] {
			let msg = RequestOk {
				request_id: Some(RequestId(9)),
				largest: Some(Location { group: 2, object: 3 }),
				properties: Properties::default(),
			};

			let encoded = encode_message(&msg, version);
			let decoded: RequestOk = decode_message(&encoded, version).unwrap();

			assert_eq!(decoded.request_id, Some(RequestId(9)), "{version}");
			assert_eq!(decoded.largest, Some(Location { group: 2, object: 3 }), "{version}");
		}
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
