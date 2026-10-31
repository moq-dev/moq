//! IETF moq-transport publish namespace messages

use crate::{Epoch, Path, coding::*, ietf::RequestId};

use super::Message;
use super::namespace::{decode_namespace, encode_namespace};
use super::token::RequestToken;
use super::{cluster, epoch};

use super::Version;

/// Which advertisement extensions a session negotiated: what its PUBLISH_NAMESPACE and
/// NAMESPACE messages carry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Negotiated {
	/// MoQ Cluster: every advertisement carries HOP_PATH, and may carry ROUTE_COST.
	pub cluster: bool,
	/// MoQ Epoch: an advertisement may carry EPOCH.
	pub epoch: bool,
}

impl Negotiated {
	/// Whether NAMESPACE takes the extended form, with a Parameters field.
	pub fn extended(&self) -> bool {
		self.cluster || self.epoch
	}
}

/// The extension parameters one advertisement carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdvertParams {
	/// The MoQ Cluster parameters (see [`cluster`]). `Some` exactly on a session that
	/// negotiated the extension.
	pub cluster: Option<cluster::Advert>,

	/// The MoQ Epoch parameter: the publisher instance the route serves, if it names one.
	/// Only on a session that negotiated the extension.
	pub epoch: Option<Epoch>,
}

/// PublishNamespace message (0x06)
/// Sent by the publisher to announce the availability of a namespace.
#[derive(Clone, Debug)]
pub struct PublishNamespace {
	pub request_id: RequestId,
	pub track_namespace: Path,

	/// The extension parameters, which a session that negotiated none leaves empty.
	pub params: AdvertParams,

	/// The `AUTHORIZATION TOKEN` (0x03) this request presents. At most one is
	/// accepted: decoding refuses a repeated or second token.
	pub authorization_token: Option<RequestToken>,
}

impl PublishNamespace {
	/// Decode the message body, expecting the parameters of the extensions the session
	/// negotiated.
	///
	/// The negotiation is session state rather than anything in the message, so the
	/// caller supplies it. A negotiated cluster session that omits HOP_PATH, or a
	/// parameter of an extension the session did not negotiate, is a protocol violation,
	/// which surfaces here as [`DecodeError::InvalidValue`].
	pub fn decode_body(r: &mut Decoder<'_>, version: Version, negotiated: Negotiated) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		let track_namespace = decode_namespace(r)?;

		decode_params!(r, version,
			0x03 => authorization_token: Vec<super::token::RequestToken>,
			cluster::HOP_PATH => hops: Option<cluster::HopPath>,
			cluster::ROUTE_COST => cost: Option<u64>,
			epoch::EPOCH_PARAM => epoch: Option<Epoch> where negotiated.epoch,
		);
		let params = AdvertParams {
			cluster: cluster_params(negotiated.cluster, hops, cost)?,
			epoch,
		};
		// Last, so a malformed field closes the session before a token refusal.
		let authorization_token = super::token::single(authorization_token)?;

		Ok(Self {
			request_id,
			track_namespace,
			params,
			authorization_token,
		})
	}
}

impl<'a> Message<'a> for PublishNamespace {
	const ID: u64 = 0x06;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_namespace(w, &self.track_namespace)?;
		// ROUTE_COST is optional and absent means 0, so a free path sends nothing.
		let hops = self.params.cluster.as_ref().map(|advert| advert.hops.clone());
		let cost = self
			.params
			.cluster
			.as_ref()
			.and_then(|advert| (advert.cost != 0).then_some(advert.cost));
		encode_params!(w, version,
			0x03 => self.authorization_token.clone(),
			cluster::HOP_PATH => hops,
			cluster::ROUTE_COST => cost,
			epoch::EPOCH_PARAM => self.params.epoch.clone(),
		);
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		Self::decode_body(r, version, Negotiated::default())
	}
}

/// The MoQ Cluster parameters of an advertisement, given what arrived.
///
/// A negotiated session requires HOP_PATH, and ROUTE_COST absent means 0. An endpoint
/// must not send either on a session that did not negotiate, so either is a violation.
fn cluster_params(
	negotiated: bool,
	hops: Option<cluster::HopPath>,
	cost: Option<u64>,
) -> Result<Option<cluster::Advert>, DecodeError> {
	match negotiated {
		true => Ok(Some(cluster::Advert {
			hops: hops.ok_or(DecodeError::InvalidValue)?,
			cost: cost.unwrap_or(0),
		})),
		false if hops.is_some() || cost.is_some() => Err(DecodeError::InvalidValue),
		false => Ok(None),
	}
}

/// REQUEST_UPDATE (0x02) on a PUBLISH_NAMESPACE: the cluster parameters that changed
/// (draft-lcurley-moq-cluster, Updating an Advertisement), or a renewed request token.
///
/// An omitted parameter keeps its value (moq-transport Section 9.5), so a cost that
/// dropped to 0 is sent as an explicit 0, unlike the advertisement itself where absent
/// means 0.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishNamespaceUpdate {
	/// The update's own Request ID; every REQUEST_UPDATE consumes one.
	pub request_id: RequestId,
	/// HOP_PATH, when the route changed.
	pub hops: Option<cluster::HopPath>,
	/// ROUTE_COST, when the cost changed.
	pub cost: Option<u64>,
	/// The `AUTHORIZATION TOKEN` (0x03) renewing the advertisement's credential, if any. At
	/// most one is accepted: decoding refuses a repeated or second token.
	pub authorization_token: Option<RequestToken>,
}

impl PublishNamespaceUpdate {
	/// The update that moves a peer holding `held` to `next`: only what changed.
	pub fn between(request_id: RequestId, held: &cluster::Advert, next: &cluster::Advert) -> Self {
		Self {
			request_id,
			hops: (held.hops != next.hops).then(|| next.hops.clone()),
			cost: (held.cost != next.cost).then_some(next.cost),
			authorization_token: None,
		}
	}

	/// The advertisement after this update is applied to `held`.
	pub fn apply(&self, held: &cluster::Advert) -> cluster::Advert {
		cluster::Advert {
			hops: self.hops.clone().unwrap_or_else(|| held.hops.clone()),
			cost: self.cost.unwrap_or(held.cost),
		}
	}
}

impl<'a> Message<'a> for PublishNamespaceUpdate {
	const ID: u64 = 0x02;

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.request_id.encode(w, version)?;
		encode_params!(w, version,
			0x03 => self.authorization_token.clone(),
			cluster::HOP_PATH => self.hops,
			cluster::ROUTE_COST => self.cost,
		);
		Ok(())
	}

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let request_id = RequestId::decode(r, version)?;
		decode_params!(r, version,
			0x03 => authorization_token: Vec<super::token::RequestToken>,
			cluster::HOP_PATH => hops: Option<cluster::HopPath>,
			cluster::ROUTE_COST => cost: Option<u64>,
		);
		let authorization_token = super::token::single(authorization_token)?;
		Ok(Self {
			request_id,
			hops,
			cost,
			authorization_token,
		})
	}
}

/// Write the Parameters field of an extended NAMESPACE.
///
/// ROUTE_COST is optional and absent means 0, so a free path sends nothing.
pub(super) fn encode_advert_params(
	w: &mut Encoder<'_>,
	version: Version,
	params: &AdvertParams,
) -> Result<(), EncodeError> {
	let hops = params.cluster.as_ref().map(|advert| advert.hops.clone());
	let cost = params
		.cluster
		.as_ref()
		.and_then(|advert| (advert.cost != 0).then_some(advert.cost));
	encode_params!(w, version,
		cluster::HOP_PATH => hops,
		cluster::ROUTE_COST => cost,
		epoch::EPOCH_PARAM => params.epoch.clone(),
	);
	Ok(())
}

/// Read the Parameters field of an extended NAMESPACE. See [`encode_advert_params`].
pub(super) fn decode_advert_params(
	r: &mut Decoder<'_>,
	version: Version,
	negotiated: Negotiated,
) -> Result<AdvertParams, DecodeError> {
	decode_params!(r, version,
		cluster::HOP_PATH => hops: Option<cluster::HopPath>,
		cluster::ROUTE_COST => cost: Option<u64>,
		epoch::EPOCH_PARAM => epoch: Option<Epoch> where negotiated.epoch,
	);

	Ok(AdvertParams {
		cluster: cluster_params(negotiated.cluster, hops, cost)?,
		epoch,
	})
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
			params: Default::default(),
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
			params: Default::default(),
			authorization_token: None,
		};

		let encoded = encode_message(&msg, Version::Draft18);
		let decoded: PublishNamespace = decode_message(&encoded, Version::Draft18).unwrap();

		assert_eq!(decoded.request_id, RequestId(5));
		assert_eq!(decoded.track_namespace.as_str(), "v18/broadcast");
	}

	fn hop_path(ids: &[u64]) -> cluster::HopPath {
		let hops: Vec<crate::Hop> = ids.iter().map(|&id| crate::Hop::new(id).unwrap()).collect();
		cluster::HopPath::new(crate::Hops::try_from(hops).unwrap())
	}

	/// An update carries only what changed, and a cost that dropped to 0 is an explicit 0:
	/// REQUEST_UPDATE keeps an omitted parameter, so leaving it out would keep the old price.
	#[test]
	fn update_carries_only_the_change_and_an_explicit_zero() {
		let held = cluster::Advert {
			hops: hop_path(&[7, 9]),
			cost: 4,
		};
		let cheaper = cluster::Advert {
			cost: 0,
			..held.clone()
		};

		let msg = PublishNamespaceUpdate::between(RequestId(2), &held, &cheaper);
		assert_eq!(msg.hops, None, "an unchanged path is omitted");
		assert_eq!(msg.cost, Some(0), "a cost of 0 is sent explicitly");

		for version in [Version::Draft18, Version::Draft20, Version::Draft22] {
			let encoded = encode_message(&msg, version);
			let decoded: PublishNamespaceUpdate = decode_message(&encoded, version).unwrap();
			assert_eq!(decoded, msg, "{version}");
			assert_eq!(decoded.apply(&held), cheaper, "{version}");
		}

		let rerouted = cluster::Advert {
			hops: hop_path(&[7, 11]),
			cost: 4,
		};
		let msg = PublishNamespaceUpdate::between(RequestId(4), &held, &rerouted);
		assert_eq!(msg.hops, Some(hop_path(&[7, 11])));
		assert_eq!(msg.cost, None, "an unchanged cost is omitted");
		let decoded: PublishNamespaceUpdate =
			decode_message(&encode_message(&msg, Version::Draft18), Version::Draft18).unwrap();
		assert_eq!(decoded.apply(&held), rerouted);
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
