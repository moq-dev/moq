//! IETF moq-transport subscribe namespace messages

use std::borrow::Cow;

use crate::{Path, coding::*, ietf::RequestId};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

/// SubscribeNamespace message (0x11)
/// In v16, this moves from the control stream to its own bidirectional stream.
#[derive(Clone, Debug)]
pub struct SubscribeNamespace<'a> {
	pub request_id: RequestId,
	pub namespace: Path<'a>,
	/// v16: Subscribe Options (default 0x01 = NAMESPACE only)
	pub subscribe_options: u64,
}

impl Message for SubscribeNamespace<'_> {
	const ID: u64 = 0x11;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			0u64.encode(w, version)?; // required_request_id_delta = 0
		}
		encode_namespace(w, &self.namespace, version)?;
		if version == Version::Draft16 || version == Version::Draft17 {
			self.subscribe_options.encode(w, version)?;
		}
		encode_params!(w, version,);
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = u64::decode(r, version)?;
		}
		let namespace = decode_namespace(r, version)?;
		let subscribe_options = match version {
			Version::Draft16 | Version::Draft17 => u64::decode(r, version)?,
			Version::Draft14 | Version::Draft15 => 0x01,
		};

		// Ignore parameters
		decode_params!(r, version,);

		Ok(Self {
			namespace,
			request_id,
			subscribe_options,
		})
	}
}

/// SubscribeNamespaceOk message (0x12) — v14 only
#[derive(Clone, Debug)]
pub struct SubscribeNamespaceOk {
	pub request_id: RequestId,
}

impl Message for SubscribeNamespaceOk {
	const ID: u64 = 0x12;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

/// SubscribeNamespaceError message (0x13) — v14 only
#[derive(Clone, Debug)]
pub struct SubscribeNamespaceError<'a> {
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for SubscribeNamespaceError<'_> {
	const ID: u64 = 0x13;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		self.error_code.encode(w, version)?;
		self.reason_phrase.encode(w, version)?;
		Ok(())
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

/// UnsubscribeNamespace message (0x14) — v14/v15 only (v16 uses stream close)
#[derive(Clone, Debug)]
pub struct UnsubscribeNamespace {
	pub request_id: RequestId,
}

impl Message for UnsubscribeNamespace {
	const ID: u64 = 0x14;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

/// NAMESPACE message (0x08) — v16 only, sent on SUBSCRIBE_NAMESPACE bidi stream
/// Indicates a namespace suffix matching the subscribed prefix is active.
#[derive(Clone, Debug)]
pub struct Namespace<'a> {
	pub suffix: Path<'a>,
}

impl Message for Namespace<'_> {
	const ID: u64 = 0x08;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		encode_namespace(w, &self.suffix, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let suffix = decode_namespace(r, version)?;
		Ok(Self { suffix })
	}
}

/// PUBLISH_BLOCKED message (0x0F) — draft-17 only
/// Indicates a track within a namespace is blocked from publishing.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Will be used in Phase 3 bidi stream handling
pub struct PublishBlocked<'a> {
	pub suffix: Path<'a>,
	pub track_name: Cow<'a, str>,
}

impl Message for PublishBlocked<'_> {
	const ID: u64 = 0x0F;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		assert!(version == Version::Draft17, "PublishBlocked is draft17 only");
		encode_namespace(w, &self.suffix, version)?;
		self.track_name.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if version != Version::Draft17 {
			return Err(DecodeError::Unsupported);
		}
		let suffix = decode_namespace(r, version)?;
		let track_name = Cow::<str>::decode(r, version)?;
		Ok(Self { suffix, track_name })
	}
}

/// NAMESPACE_DONE message (0x0E) — v16 only, sent on SUBSCRIBE_NAMESPACE bidi stream
/// Indicates a namespace suffix matching the subscribed prefix is no longer active.
#[derive(Clone, Debug)]
pub struct NamespaceDone<'a> {
	pub suffix: Path<'a>,
}

impl Message for NamespaceDone<'_> {
	const ID: u64 = 0x0E;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		encode_namespace(w, &self.suffix, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let suffix = decode_namespace(r, version)?;
		Ok(Self { suffix })
	}
}
