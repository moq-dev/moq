//! IETF moq-transport-14 publish namespace messages

use std::borrow::Cow;

use crate::{Path, coding::*, ietf::RequestId};

use super::Message;
use super::cluster;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

/// PublishNamespace message (0x06)
/// Sent by the publisher to announce the availability of a namespace.
#[derive(Clone, Debug)]
pub struct PublishNamespace<'a> {
	pub request_id: RequestId,
	pub track_namespace: Path<'a>,

	/// The MoQ Cluster parameters (see [`cluster`]). `Some` on a session that
	/// negotiated the extension and `None` on one that did not, which is what decides
	/// whether they appear on the wire at all.
	pub cluster: Option<cluster::Advert>,
}

impl PublishNamespace<'_> {
	/// Decode the message body, expecting the cluster parameters when the session
	/// negotiated the extension.
	///
	/// The negotiation is session state rather than anything in the message, so the
	/// caller supplies it. A negotiated session that omits HOP_PATH is a protocol
	/// violation, which surfaces here as [`DecodeError::InvalidValue`].
	pub fn decode_body<R: bytes::Buf>(r: &mut R, version: Version, cluster: bool) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = u64::decode(r, version)?;
		}
		let track_namespace = decode_namespace(r, version)?;
		let cluster = decode_cluster_params(r, version, cluster)?;

		Ok(Self {
			request_id,
			track_namespace,
			cluster,
		})
	}
}

impl Message for PublishNamespace<'_> {
	const ID: u64 = 0x06;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			0u64.encode(w, version)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}
		encode_namespace(w, &self.track_namespace, version)?;
		encode_cluster_params(w, version, self.cluster.as_ref())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Self::decode_body(r, version, false)
	}
}

/// Write the Parameters field of an advertisement.
///
/// On a session that negotiated the MoQ Cluster extension every advertisement carries
/// HOP_PATH; ROUTE_COST is optional and absent means 0, so a free path sends nothing.
pub(super) fn encode_cluster_params<W: bytes::BufMut>(
	w: &mut W,
	version: Version,
	advert: Option<&cluster::Advert>,
) -> Result<(), EncodeError> {
	match advert {
		Some(advert) => {
			let cost = (advert.cost != 0).then_some(advert.cost);
			encode_params!(w, version,
				cluster::HOP_PATH => advert.hops,
				cluster::ROUTE_COST => cost,
			);
		}
		None => encode_params!(w, version,),
	}
	Ok(())
}

/// Read the Parameters field of an advertisement. See [`encode_cluster_params`].
pub(super) fn decode_cluster_params<R: bytes::Buf>(
	r: &mut R,
	version: Version,
	negotiated: bool,
) -> Result<Option<cluster::Advert>, DecodeError> {
	if !negotiated {
		// An endpoint must not append these on a session that did not negotiate the
		// extension, and we know no other parameter here, so any is a violation.
		decode_params!(r, version,);
		return Ok(None);
	}

	decode_params!(r, version,
		cluster::HOP_PATH => hops: Option<cluster::HopPath>,
		cluster::ROUTE_COST => cost: Option<u64>,
	);

	Ok(Some(cluster::Advert {
		hops: hops.ok_or(DecodeError::InvalidValue)?,
		cost: cost.unwrap_or(0),
	}))
}

/// PublishNamespaceOk message (0x07)
#[derive(Clone, Debug)]
pub struct PublishNamespaceOk {
	pub request_id: RequestId,
}

impl Message for PublishNamespaceOk {
	const ID: u64 = 0x07;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		Ok(Self { request_id })
	}
}

/// PublishNamespaceError message (0x08)
#[derive(Clone, Debug)]
pub struct PublishNamespaceError<'a> {
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for PublishNamespaceError<'_> {
	const ID: u64 = 0x08;

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

/// PublishNamespaceDone message (0x09)
/// v14/v15: uses track_namespace. v16: uses request_id.
#[derive(Clone, Debug)]
pub struct PublishNamespaceDone<'a> {
	/// v14/v15: the namespace being unannounced
	pub track_namespace: Path<'a>,
	/// v16: the request ID of the original PublishNamespace
	pub request_id: RequestId,
}

impl Message for PublishNamespaceDone<'_> {
	const ID: u64 = 0x09;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft14 | Version::Draft15 => {
				encode_namespace(w, &self.track_namespace, version)?;
			}
			Version::Draft16 => {
				self.request_id.encode(w, version)?;
			}
			_ => return Err(EncodeError::Version),
		}
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft14 | Version::Draft15 => {
				let track_namespace = decode_namespace(r, version)?;
				Ok(Self {
					track_namespace,
					request_id: RequestId(0),
				})
			}
			Version::Draft16 => {
				let request_id = RequestId::decode(r, version)?;
				Ok(Self {
					track_namespace: Path::default(),
					request_id,
				})
			}
			_ => Err(DecodeError::Version),
		}
	}
}

/// PublishNamespaceCancel message (0x0c)
/// v14/v15: uses track_namespace. v16: uses request_id.
#[derive(Clone, Debug)]
pub struct PublishNamespaceCancel<'a> {
	/// v14/v15: the namespace being cancelled
	pub track_namespace: Path<'a>,
	/// v16: the request ID of the original PublishNamespace
	pub request_id: RequestId,
	pub error_code: u64,
	pub reason_phrase: Cow<'a, str>,
}

impl Message for PublishNamespaceCancel<'_> {
	const ID: u64 = 0x0c;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Draft14 | Version::Draft15 => {
				encode_namespace(w, &self.track_namespace, version)?;
			}
			Version::Draft16 => {
				self.request_id.encode(w, version)?;
			}
			_ => {
				return Err(EncodeError::Version);
			}
		}
		self.error_code.encode(w, version)?;
		self.reason_phrase.encode(w, version)?;
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let (track_namespace, request_id) = match version {
			Version::Draft14 | Version::Draft15 => {
				let track_namespace = decode_namespace(r, version)?;
				(track_namespace, RequestId(0))
			}
			Version::Draft16 => {
				let request_id = RequestId::decode(r, version)?;
				(Path::default(), request_id)
			}
			_ => {
				return Err(DecodeError::Version);
			}
		};
		let error_code = u64::decode(r, version)?;
		let reason_phrase = Cow::<str>::decode(r, version)?;
		Ok(Self {
			track_namespace,
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
		msg.encode_msg(&mut buf, version).unwrap();
		buf.to_vec()
	}

	fn decode_message<M: Message>(bytes: &[u8], version: Version) -> Result<M, DecodeError> {
		let mut buf = bytes::Bytes::from(bytes.to_vec());
		M::decode_msg(&mut buf, version)
	}

	#[test]
	fn test_announce_round_trip() {
		let msg = PublishNamespace {
			request_id: RequestId(1),
			track_namespace: Path::new("test/broadcast"),
			cluster: None,
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: PublishNamespace = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.track_namespace.as_str(), "test/broadcast");
	}

	#[test]
	fn test_announce_error() {
		let msg = PublishNamespaceError {
			request_id: RequestId(1),
			error_code: 404,
			reason_phrase: "Unauthorized".into(),
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: PublishNamespaceError = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.error_code, 404);
		assert_eq!(decoded.reason_phrase, "Unauthorized");
	}

	#[test]
	fn test_unannounce_v14() {
		let msg = PublishNamespaceDone {
			track_namespace: Path::new("old/stream"),
			request_id: RequestId(0),
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: PublishNamespaceDone = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.track_namespace.as_str(), "old/stream");
	}

	#[test]
	fn test_unannounce_v16() {
		let msg = PublishNamespaceDone {
			track_namespace: Path::default(),
			request_id: RequestId(42),
		};

		let encoded = encode_message(&msg, Version::Draft16);
		let decoded: PublishNamespaceDone = decode_message(&encoded, Version::Draft16).unwrap();

		assert_eq!(decoded.request_id, RequestId(42));
	}

	#[test]
	fn test_announce_cancel_v14() {
		let msg = PublishNamespaceCancel {
			track_namespace: Path::new("canceled"),
			request_id: RequestId(0),
			error_code: 1,
			reason_phrase: "Shutdown".into(),
		};

		let encoded = encode_message(&msg, Version::Draft14);
		let decoded: PublishNamespaceCancel = decode_message(&encoded, Version::Draft14).unwrap();

		assert_eq!(decoded.track_namespace.as_str(), "canceled");
		assert_eq!(decoded.error_code, 1);
		assert_eq!(decoded.reason_phrase, "Shutdown");
	}

	#[test]
	fn test_announce_cancel_v16() {
		let msg = PublishNamespaceCancel {
			track_namespace: Path::default(),
			request_id: RequestId(7),
			error_code: 1,
			reason_phrase: "Shutdown".into(),
		};

		let encoded = encode_message(&msg, Version::Draft16);
		let decoded: PublishNamespaceCancel = decode_message(&encoded, Version::Draft16).unwrap();

		assert_eq!(decoded.request_id, RequestId(7));
		assert_eq!(decoded.error_code, 1);
		assert_eq!(decoded.reason_phrase, "Shutdown");
	}

	#[test]
	fn test_publish_namespace_v17_round_trip() {
		let msg = PublishNamespace {
			request_id: RequestId(5),
			track_namespace: Path::new("v17/broadcast"),
			cluster: None,
		};

		let encoded = encode_message(&msg, Version::Draft17);
		let decoded: PublishNamespace = decode_message(&encoded, Version::Draft17).unwrap();

		assert_eq!(decoded.request_id, RequestId(5));
		assert_eq!(decoded.track_namespace.as_str(), "v17/broadcast");
	}

	#[test]
	fn test_publish_namespace_v18_round_trip() {
		let msg = PublishNamespace {
			request_id: RequestId(5),
			track_namespace: Path::new("v18/broadcast"),
			cluster: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: PublishNamespace = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(5));
		assert_eq!(decoded.track_namespace.as_str(), "v18/broadcast");
	}

	#[test]
	fn test_publish_namespace_done_v18_rejected() {
		let msg = PublishNamespaceDone {
			track_namespace: Path::default(),
			request_id: RequestId(42),
		};

		let mut buf = BytesMut::new();
		assert!(msg.encode_msg(&mut buf, Version::Draft18).is_err());
	}

	#[test]
	fn test_publish_namespace_done_v17_rejected() {
		let msg = PublishNamespaceDone {
			track_namespace: Path::default(),
			request_id: RequestId(42),
		};

		let mut buf = BytesMut::new();
		assert!(msg.encode_msg(&mut buf, Version::Draft17).is_err());
	}

	#[test]
	fn test_publish_namespace_cancel_v17_rejected() {
		let msg = PublishNamespaceCancel {
			track_namespace: Path::default(),
			request_id: RequestId(7),
			error_code: 1,
			reason_phrase: "Shutdown".into(),
		};

		let mut buf = BytesMut::new();
		assert!(msg.encode_msg(&mut buf, Version::Draft17).is_err());
	}

	#[test]
	fn test_announce_rejects_parameters() {
		#[rustfmt::skip]
		let invalid_bytes = vec![
			0x01, // namespace length
			0x04, 0x74, 0x65, 0x73, 0x74, // "test"
			0x01, // INVALID: num_params = 1
		];

		let result: Result<PublishNamespace, _> = decode_message(&invalid_bytes, Version::Draft14);
		assert!(result.is_err());
	}
}
