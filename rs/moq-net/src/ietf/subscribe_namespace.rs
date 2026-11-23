//! IETF moq-transport subscribe namespace messages

use crate::{Path, coding::*, ietf::RequestId};

use super::Message;
use super::hidden::HIDDEN_PARAM;
use super::namespace::{decode_namespace, encode_namespace};
use super::publish_namespace::{AdvertParams, Negotiated};

use super::Version;

/// SUBSCRIBE_NAMESPACE message (type 0x50).
#[derive(Clone, Debug)]
pub struct SubscribeNamespace {
	/// The credential authorizing this request alone.
	pub authorization_token: Option<super::token::RequestToken>,
	pub request_id: RequestId,
	pub namespace: Path,
	/// MoQ Hidden: also advertise hidden namespaces. Only sent to a peer that declared
	/// the extension; absent means false.
	pub hidden: bool,
}

/// The HIDDEN parameter value to write: absent unless opted in.
fn hidden_param(hidden: bool) -> Option<u64> {
	hidden.then_some(1)
}

/// Read the HIDDEN parameter value. 0 and absent both mean not opted in.
fn hidden_from_param(value: Option<u64>) -> Result<bool, DecodeError> {
	match value {
		None | Some(0) => Ok(false),
		Some(1) => Ok(true),
		Some(_) => Err(DecodeError::InvalidValue),
	}
}

impl<'a> Message<'a> for SubscribeNamespace {
	const ID: u64 = 0x50;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_namespace(w, &self.namespace)?;
		encode_params!(w, version, 0x03 => self.authorization_token.clone(), HIDDEN_PARAM => hidden_param(self.hidden));
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let namespace = decode_namespace(r)?;
		decode_params!(r, version,
			0x03 => authorization_token: Vec<super::token::RequestToken>,
			HIDDEN_PARAM => hidden: Option<u64>,
		);
		let hidden = hidden_from_param(hidden)?;
		// Last, so a malformed field closes the session before a token refusal.
		let authorization_token = super::token::single(authorization_token)?;

		Ok(Self {
			authorization_token,
			request_id,
			namespace,
			hidden,
		})
	}
}

/// NAMESPACE message (0x08), sent on the SUBSCRIBE_NAMESPACE bidi stream.
/// Indicates a namespace suffix matching the subscribed prefix is active.
///
/// Through draft-22 the base message carries only the suffix, and a session that negotiated
/// the MoQ Cluster extension appends a Parameters field carrying HOP_PATH and ROUTE_COST
/// (see [`super::cluster`]). Draft-23 (section 9.15) gives every NAMESPACE that field, and
/// defines no parameter for it: without an extension it is an empty block.
#[derive(Clone, Debug)]
pub struct Namespace {
	pub suffix: Path,

	/// The extension parameters. `Some` selects the extended form; `None` the base one.
	/// An endpoint must not append them on a session without MoQ Cluster.
	pub params: Option<AdvertParams>,
}

impl Namespace {
	/// Whether every NAMESPACE carries a Parameters field (draft-23 section 9.15), not only
	/// one on a session whose extension defines it.
	fn has_params(version: Version) -> bool {
		!matches!(version, Version::Draft18 | Version::Draft20 | Version::Draft22)
	}

	/// Decode the message body, expecting the extended form when the session negotiated
	/// an extension that defines it. See [`super::PublishNamespace::decode_body`].
	pub fn decode_body(r: &mut Decoder<'_>, version: Version, negotiated: Negotiated) -> Result<Self, DecodeError> {
		let suffix = decode_namespace(r)?;

		let params = match negotiated.extended() {
			true => Some(super::publish_namespace::decode_advert_params(r, version, negotiated)?),
			// The draft defines no parameter for NAMESPACE and an unknown one cannot be
			// skipped (section 9.19), so without an extension the block must be empty.
			false if Self::has_params(version) => {
				decode_params!(r, version,);
				None
			}
			// Through draft-22 the base form has no Parameters field at all, so there is
			// nothing to read (and nothing to reject).
			false => None,
		};

		Ok(Self { suffix, params })
	}
}

impl<'a> Message<'a> for Namespace {
	const ID: u64 = 0x08;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		encode_namespace(w, &self.suffix)?;
		match &self.params {
			Some(params) => super::publish_namespace::encode_advert_params(w, version, params),
			None if Self::has_params(version) => {
				encode_params!(w, version,);
				Ok(())
			}
			None => Ok(()),
		}
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		Self::decode_body(r, version, Negotiated::default())
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
	use super::super::cluster;
	use super::*;

	fn body<'m, M: Message<'m>>(msg: &M, version: Version) -> Vec<u8> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		buf.to_vec()
	}

	fn hop_path(ids: &[u64]) -> cluster::HopPath {
		let hops = ids.iter().map(|&id| crate::Hop::new(id).unwrap()).collect::<Vec<_>>();
		cluster::HopPath::new(crate::Hops::try_from(hops).unwrap())
	}

	const CLUSTER: Negotiated = Negotiated { cluster: true };
	fn decode(bytes: Vec<u8>, version: Version, negotiated: Negotiated) -> Result<(Namespace, bool), DecodeError> {
		let mut buf = bytes::Bytes::from(bytes);
		let decoded = crate::coding::decode_buf(&mut buf, version, |r, v| Namespace::decode_body(r, v, negotiated))?;
		Ok((decoded, buf.is_empty()))
	}

	fn clustered(hops: &[u64], cost: u64) -> Option<AdvertParams> {
		Some(AdvertParams {
			cluster: Some(cluster::Advert {
				hops: hop_path(hops),
				cost,
			}),
		})
	}

	/// The extended NAMESPACE appends a Parameters field carrying the path and cost;
	/// the base one has no such field at all.
	#[test]
	fn cluster_namespace_round_trips() {
		let version = Version::Draft18;
		let msg = Namespace {
			suffix: Path::from_static("alice.hang"),
			params: clustered(&[7, 9], 5),
		};

		let encoded = body(&msg, version);
		let (decoded, consumed) = decode(encoded.clone(), version, CLUSTER).unwrap();
		assert!(consumed);
		assert_eq!(decoded.suffix.as_str(), "alice.hang");
		assert_eq!(decoded.params, msg.params);

		// The base form is strictly shorter, and reading the extended bytes as base
		// leaves the parameters behind rather than silently absorbing them.
		let base = Namespace {
			suffix: Path::from_static("alice.hang"),
			params: None,
		};
		let base_encoded = body(&base, version);
		assert!(base_encoded.len() < encoded.len());

		let (decoded, consumed) = decode(encoded, version, Negotiated::default()).unwrap();
		assert!(decoded.params.is_none());
		assert!(!consumed, "the parameters were not consumed");
	}

	/// ROUTE_COST is optional and absent means 0, so a free path sends nothing.
	#[test]
	fn cluster_namespace_omits_zero_cost() {
		let version = Version::Draft18;
		let free = Namespace {
			suffix: Path::from_static("a"),
			params: clustered(&[7], 0),
		};
		let priced = Namespace {
			suffix: Path::from_static("a"),
			params: clustered(&[7], 1),
		};
		assert!(body(&free, version).len() < body(&priced, version).len());

		let (decoded, _) = decode(body(&free, version), version, CLUSTER).unwrap();
		assert_eq!(decoded.params.unwrap().cluster.unwrap().cost, 0);
	}

	/// A negotiated session that omits HOP_PATH is a protocol violation.
	#[test]
	fn cluster_namespace_requires_hop_path() {
		let version = Version::Draft18;
		let mut buf = Vec::new();
		encode_namespace(&mut Encoder::new(&mut buf, version.into()), &Path::from_static("a")).unwrap();
		// Number of Parameters = 0.
		Encoder::new(&mut buf, version.into()).varint(0).unwrap();

		let mut bytes = bytes::Bytes::from(buf);
		assert!(matches!(
			crate::coding::decode_buf(&mut bytes, version, |r, v| Namespace::decode_body(r, v, CLUSTER)),
			Err(DecodeError::InvalidValue)
		));
	}

	/// Draft-23 (section 9.15) gives every NAMESPACE a Number of Parameters, zero without
	/// an extension. Drafts 18 to 22 keep the bare suffix.
	#[test]
	fn namespace_has_an_empty_parameter_block_at_draft23() {
		let msg = Namespace {
			suffix: Path::from_static("alice.hang"),
			params: None,
		};
		let old = body(&msg, Version::Draft22);
		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			assert_eq!(body(&msg, version), old, "{version:?} keeps the bare suffix");
			let (decoded, consumed) = decode(old.clone(), version, Negotiated::default()).unwrap();
			assert!(consumed && decoded.params.is_none());
		}

		let new = body(&msg, Version::Draft23);
		assert_eq!(new[..old.len()], old[..]);
		assert_eq!(new[old.len()..], [0], "Number of Parameters = 0");
		let (decoded, consumed) = decode(new, Version::Draft23, Negotiated::default()).unwrap();
		assert!(consumed && decoded.params.is_none());
		assert_eq!(decoded.suffix.as_str(), "alice.hang");

		// The bare suffix is not a draft-23 NAMESPACE: the count is missing.
		assert!(decode(old, Version::Draft23, Negotiated::default()).is_err());
	}

	/// The draft defines no parameter for NAMESPACE and an unknown one cannot be skipped
	/// (section 9.19), so one arriving without an extension is refused.
	#[test]
	fn namespace_refuses_a_parameter_at_draft23_without_an_extension() {
		let version = Version::Draft23;
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, version.into());
		encode_namespace(&mut w, &Path::from_static("a")).unwrap();
		// Number of Parameters = 1, then an undefined type and a one-byte value.
		w.varint(1).unwrap();
		w.varint(0x3e).unwrap();
		w.varint(0).unwrap();

		assert!(matches!(
			decode(buf, version, Negotiated::default()),
			Err(DecodeError::InvalidValue)
		));
	}

	/// With MoQ Cluster negotiated the block is the extension's, at draft-23 as before it.
	#[test]
	fn cluster_namespace_round_trips_at_draft23() {
		let version = Version::Draft23;
		let msg = Namespace {
			suffix: Path::from_static("alice.hang"),
			params: clustered(&[7, 9], 5),
		};
		assert_eq!(body(&msg, version), body(&msg, Version::Draft22));

		let (decoded, consumed) = decode(body(&msg, version), version, CLUSTER).unwrap();
		assert!(consumed);
		assert_eq!(decoded.params, msg.params);
	}

	/// An endpoint must not append the parameters on a session that did not negotiate,
	/// so a receiver that sees them there rejects the message.
	#[test]
	fn cluster_params_rejected_when_not_negotiated() {
		let version = Version::Draft18;
		let msg = super::super::PublishNamespace {
			request_id: RequestId(1),
			track_namespace: Path::from_static("a"),
			params: AdvertParams {
				cluster: Some(cluster::Advert {
					hops: hop_path(&[7]),
					cost: 0,
				}),
			},
			authorization_token: None,
		};

		let mut buf = Vec::new();
		msg.encode_msg(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut bytes = bytes::Bytes::from(buf);
		assert!(
			crate::coding::decode_buf(&mut bytes, version, |r, v| super::super::PublishNamespace::decode_body(
				r,
				v,
				Negotiated::default()
			))
			.is_err()
		);
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
			hidden: false,
		};
		assert_eq!(body(&msg, Version::Draft18), vec![0x00, 0x00, 0x00]);
	}

	#[test]
	fn round_trips() {
		let msg = SubscribeNamespace {
			authorization_token: None,
			request_id: RequestId(4),
			namespace: Path::from_static("example/meeting"),
			hidden: false,
		};
		let mut buf = bytes::Bytes::from(body(&msg, Version::Draft18));
		let decoded =
			crate::coding::decode_buf(&mut buf, Version::Draft18, |r, v| SubscribeNamespace::decode_msg(r, v)).unwrap();
		assert!(buf.is_empty());
		assert_eq!(decoded.request_id, RequestId(4));
		assert_eq!(decoded.namespace.as_str(), "example/meeting");
	}

	/// The opt-in round trips on every draft, and absent decodes as not opted in.
	#[test]
	fn hidden_round_trips() {
		for hidden in [false, true] {
			let msg = SubscribeNamespace {
				authorization_token: None,
				request_id: RequestId(4),
				namespace: Path::from_static("example"),
				hidden,
			};
			let mut buf = bytes::Bytes::from(body(&msg, Version::Draft18));
			assert_eq!(
				crate::coding::decode_buf(&mut buf, Version::Draft18, |r, v| SubscribeNamespace::decode_msg(r, v))
					.unwrap()
					.hidden,
				hidden
			);
		}
	}
}
