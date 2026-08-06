//! IETF moq-transport subscribe namespace messages

use std::borrow::Cow;

use crate::{Path, coding::*, ietf::RequestId};

use super::Message;
use super::cluster;
use super::namespace::{decode_namespace, encode_namespace};

use super::Version;

/// SUBSCRIBE_TRACKS message ID (0x51) introduced in draft-18 (#1542).
///
/// moq-lite does not implement PUBLISH replication through a CDN, which is the
/// only thing that SUBSCRIBE_TRACKS enables (subscribing to all tracks under a
/// prefix). If a peer sends this we fail the session loudly rather than
/// silently ignoring it, since ignoring would leave the peer waiting forever
/// for a REQUEST_OK.
pub const SUBSCRIBE_TRACKS_ID: u64 = 0x51;

/// True for the drafts that use the legacy 0x11 SUBSCRIBE_NAMESPACE message.
/// Draft-18+ uses the renumbered 0x50 message instead.
fn is_legacy_version(version: Version) -> bool {
	matches!(
		version,
		Version::Draft14 | Version::Draft15 | Version::Draft16 | Version::Draft17
	)
}

/// SUBSCRIBE_NAMESPACE message (draft-18+, type 0x50).
///
/// Draft-18 renumbered the message from 0x11 to 0x50 and dropped the Subscribe
/// Options field when it split SUBSCRIBE_TRACKS (0x51) off into its own message
/// type (#1542). Draft-14 through draft-17 use [`SubscribeNamespaceLegacy`].
#[derive(Clone, Debug)]
pub struct SubscribeNamespace<'a> {
	pub request_id: RequestId,
	pub namespace: Path<'a>,
}

impl Message for SubscribeNamespace<'_> {
	const ID: u64 = 0x50;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if is_legacy_version(version) {
			return Err(EncodeError::Version);
		}
		self.request_id.encode(w, version)?;
		encode_namespace(w, &self.namespace, version)?;
		encode_params!(w, version,);
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if is_legacy_version(version) {
			return Err(DecodeError::Version);
		}
		let request_id = RequestId::decode(r, version)?;
		let namespace = decode_namespace(r, version)?;
		decode_params!(r, version,);

		Ok(Self { request_id, namespace })
	}
}

/// SUBSCRIBE_NAMESPACE message for draft-14 through draft-17 (type 0x11).
///
/// In v16 this moves from the control stream to its own bidirectional stream.
/// Draft-16/17 carry a Subscribe Options field (NAMESPACE vs TRACKS); draft-17
/// additionally prefixes a Required Request ID delta (removed in draft-18 per
/// #1615). Draft-18+ uses [`SubscribeNamespace`].
#[derive(Clone, Debug)]
pub struct SubscribeNamespaceLegacy<'a> {
	pub request_id: RequestId,
	pub namespace: Path<'a>,
	/// v16/v17: Subscribe Options (default 0x01 = NAMESPACE only).
	pub subscribe_options: u64,
}

impl Message for SubscribeNamespaceLegacy<'_> {
	const ID: u64 = 0x11;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !is_legacy_version(version) {
			return Err(EncodeError::Version);
		}
		self.request_id.encode(w, version)?;
		if version == Version::Draft17 {
			0u64.encode(w, version)?; // required_request_id_delta = 0 (draft-17 only, removed in draft-18 per #1615)
		}
		encode_namespace(w, &self.namespace, version)?;
		if matches!(version, Version::Draft16 | Version::Draft17) {
			self.subscribe_options.encode(w, version)?;
		}
		encode_params!(w, version,);
		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !is_legacy_version(version) {
			return Err(DecodeError::Version);
		}
		let request_id = RequestId::decode(r, version)?;
		if version == Version::Draft17 {
			let _required_request_id_delta = u64::decode(r, version)?;
		}
		let namespace = decode_namespace(r, version)?;
		let subscribe_options = match version {
			Version::Draft16 | Version::Draft17 => u64::decode(r, version)?,
			_ => 0x01,
		};

		// Ignore parameters
		decode_params!(r, version,);

		Ok(Self {
			request_id,
			namespace,
			subscribe_options,
		})
	}
}

/// SubscribeNamespaceOk message (0x12): v14 only
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

/// SubscribeNamespaceError message (0x13): v14 only
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

/// UnsubscribeNamespace message (0x14): v14/v15 only (v16 uses stream close)
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

/// NAMESPACE message (0x08): v16+, sent on the SUBSCRIBE_NAMESPACE bidi stream.
/// Indicates a namespace suffix matching the subscribed prefix is active.
///
/// The base message carries only the suffix. A session that negotiated the MoQ Cluster
/// extension uses the extended form instead, which appends a Parameters field so the
/// advertisement can carry its HOP_PATH and ROUTE_COST (see [`cluster`]).
#[derive(Clone, Debug)]
pub struct Namespace<'a> {
	pub suffix: Path<'a>,

	/// The MoQ Cluster parameters. `Some` selects the extended form; `None` the base
	/// one. An endpoint must not append them on a session that did not negotiate.
	pub cluster: Option<cluster::Advert>,
}

impl Namespace<'_> {
	/// Decode the message body, expecting the extended form when the session negotiated
	/// the MoQ Cluster extension. See [`super::PublishNamespace::decode_body`].
	pub fn decode_body<R: bytes::Buf>(r: &mut R, version: Version, negotiated: bool) -> Result<Self, DecodeError> {
		let suffix = decode_namespace(r, version)?;

		// The base form has no Parameters field at all, so there is nothing to read
		// (and nothing to reject) unless the extension is on.
		let cluster = match negotiated {
			true => super::publish_namespace::decode_cluster_params(r, version, true)?,
			false => None,
		};

		Ok(Self { suffix, cluster })
	}
}

impl Message for Namespace<'_> {
	const ID: u64 = 0x08;

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		encode_namespace(w, &self.suffix, version)?;
		match &self.cluster {
			Some(advert) => super::publish_namespace::encode_cluster_params(w, version, Some(advert)),
			None => Ok(()),
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		Self::decode_body(r, version, false)
	}
}

/// PUBLISH_BLOCKED message (0x0F): draft-17 only
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

/// NAMESPACE_DONE message (0x0E): v16 only, sent on SUBSCRIBE_NAMESPACE bidi stream
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

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn body<M: Message>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version).unwrap();
		buf.to_vec()
	}

	fn hop_path(ids: &[u64]) -> cluster::HopPath {
		let hops = ids
			.iter()
			.map(|&id| crate::Origin::new(id).unwrap())
			.collect::<Vec<_>>();
		cluster::HopPath::new(crate::OriginList::try_from(hops).unwrap())
	}

	/// The extended NAMESPACE appends a Parameters field carrying the path and cost;
	/// the base one has no such field at all.
	#[test]
	fn cluster_namespace_round_trips() {
		let version = Version::Draft19;
		let msg = Namespace {
			suffix: Path::new("alice.hang"),
			cluster: Some(cluster::Advert {
				hops: hop_path(&[7, 9]),
				cost: 5,
			}),
		};

		let encoded = body(&msg, version);
		let mut buf = bytes::Bytes::from(encoded.clone());
		let decoded = Namespace::decode_body(&mut buf, version, true).unwrap();
		assert!(buf.is_empty());
		assert_eq!(decoded.suffix.as_str(), "alice.hang");
		assert_eq!(decoded.cluster, msg.cluster);

		// The base form is strictly shorter, and reading the extended bytes as base
		// leaves the parameters behind rather than silently absorbing them.
		let base = Namespace {
			suffix: Path::new("alice.hang"),
			cluster: None,
		};
		let base_encoded = body(&base, version);
		assert!(base_encoded.len() < encoded.len());

		let mut buf = bytes::Bytes::from(encoded);
		let decoded = Namespace::decode_body(&mut buf, version, false).unwrap();
		assert!(decoded.cluster.is_none());
		assert!(!buf.is_empty(), "the parameters were not consumed");
	}

	/// ROUTE_COST is optional and absent means 0, so a free path sends nothing.
	#[test]
	fn cluster_namespace_omits_zero_cost() {
		let version = Version::Draft19;
		let free = Namespace {
			suffix: Path::new("a"),
			cluster: Some(cluster::Advert {
				hops: hop_path(&[7]),
				cost: 0,
			}),
		};
		let priced = Namespace {
			suffix: Path::new("a"),
			cluster: Some(cluster::Advert {
				hops: hop_path(&[7]),
				cost: 1,
			}),
		};
		assert!(body(&free, version).len() < body(&priced, version).len());

		let mut buf = bytes::Bytes::from(body(&free, version));
		let decoded = Namespace::decode_body(&mut buf, version, true).unwrap();
		assert_eq!(decoded.cluster.unwrap().cost, 0);
	}

	/// A negotiated session that omits HOP_PATH is a protocol violation.
	#[test]
	fn cluster_namespace_requires_hop_path() {
		let version = Version::Draft19;
		let mut buf = BytesMut::new();
		encode_namespace(&mut buf, &Path::new("a"), version).unwrap();
		// Number of Parameters = 0.
		0u64.encode(&mut buf, version).unwrap();

		let mut bytes = buf.freeze();
		assert!(matches!(
			Namespace::decode_body(&mut bytes, version, true),
			Err(DecodeError::InvalidValue)
		));
	}

	/// An endpoint must not append the parameters on a session that did not negotiate,
	/// so a receiver that sees them there rejects the message.
	#[test]
	fn cluster_params_rejected_when_not_negotiated() {
		let version = Version::Draft19;
		let msg = super::super::PublishNamespace {
			request_id: RequestId(1),
			track_namespace: Path::new("a"),
			cluster: Some(cluster::Advert {
				hops: hop_path(&[7]),
				cost: 0,
			}),
		};

		let mut buf = BytesMut::new();
		msg.encode_msg(&mut buf, version).unwrap();
		let mut bytes = buf.freeze();
		assert!(super::super::PublishNamespace::decode_body(&mut bytes, version, false).is_err());
	}

	#[test]
	fn message_ids() {
		// 0x11 through draft-17 (legacy), renumbered to 0x50 in draft-18 (#1542).
		assert_eq!(SubscribeNamespaceLegacy::ID, 0x11);
		assert_eq!(SubscribeNamespace::ID, 0x50);
	}

	#[test]
	fn draft18_omits_subscribe_options() {
		// Draft-18 modern body: Request ID (0x00), empty namespace field-count
		// (0x00), Number of Parameters (0x00). No Subscribe Options field.
		let modern = SubscribeNamespace {
			request_id: RequestId(0),
			namespace: Path::default(),
		};
		assert_eq!(body(&modern, Version::Draft18), vec![0x00, 0x00, 0x00]);

		// The legacy draft-17 body keeps the options field, so it is one byte longer.
		let legacy = SubscribeNamespaceLegacy {
			request_id: RequestId(0),
			namespace: Path::default(),
			subscribe_options: 0x01,
		};
		assert!(body(&legacy, Version::Draft17).len() > body(&modern, Version::Draft18).len());
	}

	#[test]
	fn modern_round_trips() {
		let msg = SubscribeNamespace {
			request_id: RequestId(4),
			namespace: Path::new("example/meeting"),
		};
		let mut buf = bytes::Bytes::from(body(&msg, Version::Draft18));
		let decoded = SubscribeNamespace::decode_msg(&mut buf, Version::Draft18).unwrap();
		assert!(buf.is_empty());
		assert_eq!(decoded.request_id, RequestId(4));
		assert_eq!(decoded.namespace.as_str(), "example/meeting");
	}

	#[test]
	fn legacy_round_trips() {
		for version in [Version::Draft16, Version::Draft17] {
			let msg = SubscribeNamespaceLegacy {
				request_id: RequestId(4),
				namespace: Path::new("example/meeting"),
				subscribe_options: 0x01,
			};
			let mut buf = bytes::Bytes::from(body(&msg, version));
			let decoded = SubscribeNamespaceLegacy::decode_msg(&mut buf, version).unwrap();
			assert!(buf.is_empty(), "trailing bytes for {version:?}");
			assert_eq!(decoded.request_id, RequestId(4));
			assert_eq!(decoded.namespace.as_str(), "example/meeting");
			assert_eq!(decoded.subscribe_options, 0x01);
		}
	}

	#[test]
	fn rejects_wrong_version() {
		// The modern 0x50 message only exists in draft-18+.
		for version in [Version::Draft14, Version::Draft16, Version::Draft17] {
			let mut buf = bytes::Bytes::from(vec![0x00, 0x00, 0x00]);
			assert!(matches!(
				SubscribeNamespace::decode_msg(&mut buf, version),
				Err(DecodeError::Version)
			));
		}

		// The legacy 0x11 message only exists in draft-14..17.
		let mut buf = bytes::Bytes::from(vec![0x00, 0x00, 0x00]);
		assert!(matches!(
			SubscribeNamespaceLegacy::decode_msg(&mut buf, Version::Draft18),
			Err(DecodeError::Version)
		));
	}
}
