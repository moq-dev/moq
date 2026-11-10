//! IETF moq-transport publish namespace messages

use crate::{Path, coding::*, ietf::RequestId};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};
use super::token::RequestToken;

use super::Version;

/// PublishNamespace message (0x06)
/// Sent by the publisher to announce the availability of a namespace.
#[derive(Clone, Debug)]
pub struct PublishNamespace {
	pub request_id: RequestId,
	pub track_namespace: Path,

	/// The `AUTHORIZATION TOKEN` (0x03) this request presents. At most one is
	/// accepted: decoding refuses a repeated or second token.
	pub authorization_token: Option<RequestToken>,
}

impl<'a> Message<'a> for PublishNamespace {
	const ID: u64 = 0x06;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_namespace(w, &self.track_namespace)?;
		encode_params!(w, version, 0x03 => self.authorization_token.clone());
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let track_namespace = decode_namespace(r)?;
		decode_params!(r, version, 0x03 => authorization_token: Vec<super::token::RequestToken>);
		let authorization_token = super::token::single(authorization_token)?;

		Ok(Self {
			request_id,
			track_namespace,
			authorization_token,
		})
	}
}

/// REQUEST_UPDATE (0x02) on a PUBLISH_NAMESPACE: a renewed request token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishNamespaceUpdate {
	/// The update's own Request ID; every REQUEST_UPDATE consumes one.
	pub request_id: RequestId,
	/// The `AUTHORIZATION TOKEN` (0x03) renewing the advertisement's credential, if any. At
	/// most one is accepted: decoding refuses a repeated or second token.
	pub authorization_token: Option<RequestToken>,
}

impl<'a> Message<'a> for PublishNamespaceUpdate {
	const ID: u64 = 0x02;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_params!(w, version, 0x03 => self.authorization_token.clone());
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		decode_params!(r, version, 0x03 => authorization_token: Vec<super::token::RequestToken>);
		let authorization_token = super::token::single(authorization_token)?;
		Ok(Self {
			request_id,
			authorization_token,
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

	#[test]
	fn test_announce_round_trip() {
		let msg = PublishNamespace {
			request_id: RequestId(1),
			track_namespace: Path::from_static("test/broadcast"),
			authorization_token: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: PublishNamespace = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.track_namespace.as_str(), "test/broadcast");
	}

	#[test]
	fn test_publish_namespace_v18_round_trip() {
		let msg = PublishNamespace {
			request_id: RequestId(5),
			track_namespace: Path::from_static("v18/broadcast"),
			authorization_token: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: PublishNamespace = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(5));
		assert_eq!(decoded.track_namespace.as_str(), "v18/broadcast");
	}

	#[test]
	fn test_announce_rejects_parameters() {
		#[rustfmt::skip]
		let invalid_bytes = vec![
			0x01, // namespace length
			0x04, 0x74, 0x65, 0x73, 0x74, // "test"
			0x01, // INVALID: num_params = 1
		];

		let result: Result<PublishNamespace, _> = decode_message(&invalid_bytes, Version::Draft18);
		assert!(result.is_err());
	}
}
