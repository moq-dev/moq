use std::borrow::Cow;

use crate::coding::{Decode, DecodeError, Decoder, Encode, EncodeError, Encoder};

use super::active_count::ACTIVE_COUNT_PARAM;
use super::{Location, Message};

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
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(self.0)?;
		Ok(())
	}
}

impl<'a> Decode<'a, Version> for RequestId {
	fn decode(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let request_id = r.varint()?;
		Ok(Self(request_id))
	}
}

/// REQUEST_OK (0x07): the generic success response for any request, on its own stream.
#[derive(Clone, Debug)]
pub struct RequestOk {
	/// MoQ Active Count: how many NAMESPACE messages follow before the subscription is
	/// synced. Only on the answer to a SUBSCRIBE_NAMESPACE, when negotiated.
	pub active: Option<u64>,
	/// LARGEST_OBJECT: on the answer to a REQUEST_UPDATE that resumed forwarding, where the
	/// subscription now joins (draft-18 section 5.1). Absent before the track has objects.
	pub largest: Option<Location>,
}

impl<'a> Message<'a> for RequestOk {
	const ID: u64 = 0x07;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		encode_params!(w, version,
			0x09 => self.largest,
			ACTIVE_COUNT_PARAM => self.active,
		);
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		// A REQUEST_UPDATE_OK may refresh EXPIRES, which is ignored like SUBSCRIBE_OK's.
		// LARGEST_OBJECT is a MUST on a REQUEST_UPDATE_OK or TRACK_STATUS_OK once the track
		// has objects.
		decode_params!(r, version,
			0x08 => _expires: Option<u64>,
			0x09 => largest: Option<Location>,
			ACTIVE_COUNT_PARAM => active: Option<u64>,
		);
		Ok(Self { active, largest })
	}
}

/// REQUEST_ERROR (0x05): the generic error response for any request, on its own stream.
#[derive(Clone, Debug)]
pub struct RequestError<'a> {
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
	/// Retry interval in milliseconds.
	pub retry_interval: u64,
}

impl<'a> Message<'a> for RequestError<'a> {
	const ID: u64 = 0x05;

	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.varint(self.error_code)?;
		w.varint(self.retry_interval)?;
		w.string(&self.reason_phrase)?;
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let error_code = r.varint()?;
		let retry_interval = r.varint()?;
		let reason_phrase = Cow::Borrowed(r.str()?);
		Ok(Self {
			error_code,
			reason_phrase,
			retry_interval,
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn encode_message<'m, M: Message<'m>>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf.to_vec()
	}

	fn decode_message<'m, M: Message<'m>>(bytes: &'m [u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes;
		crate::coding::decode_from(&mut buf, version, |r, v| M::decode_msg(r, v))
	}

	/// A REQUEST_UPDATE_OK may refresh EXPIRES, and a REQUEST_UPDATE_OK or TRACK_STATUS_OK
	/// carries LARGEST_OBJECT; neither is rejected.
	#[test]
	fn test_request_ok_ignores_largest_object() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for bytes in [&[0x01, 0x08, 0x05][..], &[0x01, 0x09, 0x05, 0x03]] {
				let mut buf = Decoder::new(bytes, version.into());
				RequestOk::decode_msg(&mut buf, version).unwrap_or_else(|e| panic!("{version}: {e}"));
				assert!(buf.is_empty(), "{version}: trailing bytes");
			}
		}
	}

	#[test]
	fn test_request_ok_active_round_trips() {
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			for count in [None, Some(0), Some(7)] {
				let msg = RequestOk {
					largest: None,
					active: count,
				};
				let wire = encode_message(&msg, version);

				let decoded: RequestOk = decode_message(&wire, version).unwrap();
				assert_eq!(decoded.active, count, "{version:?}");
			}
		}
	}

	#[test]
	fn test_request_error_round_trip() {
		let msg = RequestError {
			error_code: 500,
			reason_phrase: "Internal error".into(),
			retry_interval: 3000,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: RequestError = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.error_code, 500);
		assert_eq!(decoded.reason_phrase, "Internal error");
		assert_eq!(decoded.retry_interval, 3000);
	}
}
