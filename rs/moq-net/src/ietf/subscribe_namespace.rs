//! IETF moq-transport subscribe namespace messages

use crate::{Path, coding::*, ietf::RequestId};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

/// SUBSCRIBE_NAMESPACE message (type 0x50).
#[derive(Clone, Debug)]
pub struct SubscribeNamespace {
	/// The credential authorizing this request alone.
	pub authorization_token: Option<super::token::RequestToken>,
	pub request_id: RequestId,
	pub namespace: Path,
}

impl<'a> Message<'a> for SubscribeNamespace {
	const ID: u64 = 0x50;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_namespace(w, &self.namespace)?;
		encode_params!(w, version, 0x03 => self.authorization_token.clone());
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let namespace = decode_namespace(r)?;
		decode_params!(r, version, 0x03 => authorization_token: Vec<super::token::RequestToken>);
		// Last, so a malformed field closes the session before a token refusal.
		let authorization_token = super::token::single(authorization_token)?;

		Ok(Self {
			authorization_token,
			request_id,
			namespace,
		})
	}
}

/// NAMESPACE message (0x08), sent on the SUBSCRIBE_NAMESPACE bidi stream.
/// Indicates a namespace suffix matching the subscribed prefix is active.
#[derive(Clone, Debug)]
pub struct Namespace {
	pub suffix: Path,
}

impl<'a> Message<'a> for Namespace {
	const ID: u64 = 0x08;

	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		encode_namespace(w, &self.suffix)
	}

	fn decode_msg(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let suffix = decode_namespace(r)?;
		Ok(Self { suffix })
	}
}

/// NAMESPACE_DONE message (0x0E), sent on the SUBSCRIBE_NAMESPACE bidi stream.
/// Indicates a namespace suffix matching the subscribed prefix is no longer active.
#[derive(Clone, Debug)]
pub struct NamespaceDone {
	pub suffix: Path,
}

impl<'a> Message<'a> for NamespaceDone {
	const ID: u64 = 0x0E;

	fn encode_msg(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		encode_namespace(w, &self.suffix)?;
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		let suffix = decode_namespace(r)?;
		Ok(Self { suffix })
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn body<'m, M: Message<'m>>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf.to_vec()
	}

	/// Draft-18 moved FORWARD (0x10) onto SUBSCRIBE_TRACKS, so it closes the session here.
	#[test]
	fn forward_is_refused() {
		let version = Version::Draft18;
		let mut buf = Vec::new();
		let w = &mut Encoder::new(&mut buf, version.into());
		RequestId(1).encode(w, version).unwrap();
		encode_namespace(w, &Path::default()).unwrap();
		// Count, key, then the raw varint.
		w.varint(1).unwrap();
		w.varint(0x10).unwrap();
		w.varint(0).unwrap();
		let mut buf = bytes::Bytes::from(buf);
		assert!(crate::coding::decode_buf(&mut buf, version, |r, v| SubscribeNamespace::decode_msg(r, v)).is_err());
	}

	#[test]
	fn message_type_code_point() {
		assert_eq!(SubscribeNamespace::ID, 0x50);
	}

	#[test]
	fn body_has_no_subscribe_options() {
		// Request ID (0x00), empty namespace field-count (0x00), Number of Parameters (0x00).
		let msg = SubscribeNamespace {
			authorization_token: None,
			request_id: RequestId(0),
			namespace: Path::default(),
		};
		assert_eq!(body(&msg, Version::Draft18), vec![0x00, 0x00, 0x00]);
	}

	#[test]
	fn round_trips() {
		let msg = SubscribeNamespace {
			authorization_token: None,
			request_id: RequestId(4),
			namespace: Path::from_static("example/meeting"),
		};
		let mut buf = bytes::Bytes::from(body(&msg, Version::Draft18));
		let decoded =
			crate::coding::decode_buf(&mut buf, Version::Draft18, |r, v| SubscribeNamespace::decode_msg(r, v)).unwrap();
		assert!(buf.is_empty());
		assert_eq!(decoded.request_id, RequestId(4));
		assert_eq!(decoded.namespace.as_str(), "example/meeting");
	}
}
