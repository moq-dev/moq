//! The MoQ Cluster extension (draft-lcurley-moq-cluster-00).
//!
//! moq-transport carries no routing information, so a mesh of relays gossiping
//! namespaces loops forever and has no basis for choosing between two peers
//! advertising the same namespace. This extension adds it:
//!
//! - each endpoint declares its own [`Origin`](crate::Origin) (Hop ID) via the
//!   RELAY_HOPS Setup Option, which is also what negotiates the extension;
//! - each endpoint prices what subscribing from it costs via the RELAY_COST Setup
//!   Option, so the two directions are priced independently;
//! - every advertisement carries the HOP_PATH it traversed and the accumulated
//!   ROUTE_COST of that path, as Key-Value-Pair message parameters.
//!
//! The semantics are the same ones moq-lite carries natively (see
//! [`crate::broadcast::Route`]); this module is only the moq-transport binding.
//! Negotiated on draft-17+ only, where SETUP is a Key-Value-Pair block.

use bytes::Buf;

use crate::coding::{Decode, DecodeError, Encode, EncodeError};
use crate::{Origin, OriginList};

use super::{Param, Version};

/// RELAY_HOPS Setup Option: the sender's own Hop ID, and the signal that it speaks
/// this extension. Odd, so the value is a length-prefixed byte string holding one varint.
pub const RELAY_HOPS: u64 = 0x40B55;

/// RELAY_COST Setup Option: what subscribing from the sender costs. Even, so the value
/// is a bare varint. Directional, so each endpoint declares its own.
pub const RELAY_COST: u64 = 0x40B56;

/// HOP_PATH message parameter: the ordered Hop IDs an advertisement traversed.
/// Odd, so the value is a length-prefixed byte string of varints.
pub const HOP_PATH: u64 = 0x40B57;

/// ROUTE_COST message parameter: the accumulated cost of the advertised path.
/// Even, so the value is a bare varint.
pub const ROUTE_COST: u64 = 0x40B58;

/// The cost of pulling across a direction nobody priced.
///
/// One, so an unpriced mesh accumulates a route cost equal to the hop count and
/// ranks routes by shortest path. Zero is meaningful and distinct from absent: it
/// makes that direction free, which is how a deployment describes two relays in the
/// same datacenter.
pub const DEFAULT_COST: u64 = 1;

/// Whether a version negotiates this extension.
///
/// Draft-17 is the first with the unified SETUP: a bare Key-Value-Pair block that a
/// new option slots into without touching the message shape. Earlier drafts exchange
/// SETUP over the legacy control stream, which this extension does not cover.
pub fn supported(version: Version) -> bool {
	!matches!(version, Version::Draft14 | Version::Draft15 | Version::Draft16)
}

/// The ordered list of Hop IDs an advertisement has traversed, from the original
/// publisher to the relay immediately upstream of the receiver.
///
/// The HOP_PATH parameter value: bare varints filling the parameter length, with no
/// count of their own. That is the only thing separating this from the moq-lite hop
/// chain, which counts its entries; the list itself is the same
/// [`OriginList`](crate::OriginList).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HopPath(OriginList);

impl HopPath {
	/// Wrap a hop chain. The list must be non-empty and free of repeated non-zero
	/// entries to be legal on the wire; [`Self::validate`] is what checks that.
	pub fn new(hops: OriginList) -> Self {
		Self(hops)
	}

	/// Borrow the hop chain.
	pub fn hops(&self) -> &OriginList {
		&self.0
	}

	/// Reject a path that cannot have come from a conforming sender: an empty list, or
	/// a non-zero Hop ID appearing twice (a loop). Duplicate zeros are legal, since 0
	/// identifies nothing.
	fn validate(&self) -> Result<(), DecodeError> {
		if self.0.is_empty() {
			return Err(DecodeError::InvalidValue);
		}

		// MAX_HOPS is 32, so the quadratic scan is cheaper than allocating a set.
		let hops = self.0.as_slice();
		for (i, hop) in hops.iter().enumerate() {
			if *hop == Origin::UNKNOWN {
				continue;
			}
			if hops[i + 1..].contains(hop) {
				return Err(DecodeError::InvalidValue);
			}
		}

		Ok(())
	}
}

impl Param for HopPath {
	fn param_encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		// The entries fill the value, so they are written with no count and framed by
		// the parameter's own length prefix.
		let mut buf = Vec::new();
		for hop in &self.0 {
			hop.encode(&mut buf, version)?;
		}
		buf.encode(w, version)
	}

	fn param_decode<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let value = Vec::<u8>::decode(r, version)?;
		let mut buf = bytes::Bytes::from(value);

		let mut hops = OriginList::new();
		while buf.has_remaining() {
			// A short read here means the entries did not exactly fill the length.
			hops.push(Origin::decode(&mut buf, version)?)?;
		}

		let path = Self(hops);
		path.validate()?;
		Ok(path)
	}
}

/// The cluster parameters carried on one advertisement.
///
/// Present on every PUBLISH_NAMESPACE and NAMESPACE of a session that negotiated the
/// extension, and on none of a session that did not.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Advert {
	/// HOP_PATH: the path this advertisement traversed, publisher first.
	pub hops: HopPath,

	/// ROUTE_COST: the marginal cost of subscribing via this advertisement. Absent on
	/// the wire means 0, so an endpoint that prices nothing sends nothing.
	pub cost: u64,
}

impl Advert {
	/// Build the advertisement to send for a route, appending our own Hop ID.
	///
	/// A relay's own ID is always the last entry, so a receiver reconstructs the full
	/// path without the sender restating it. Fails once the chain is at
	/// [`MAX_HOPS`](crate::TooManyOrigins), which a real path never reaches and a loop does.
	pub fn forward(hops: &OriginList, cost: u64, self_origin: Origin) -> Result<Self, crate::TooManyOrigins> {
		let mut hops = hops.clone();
		hops.push(self_origin)?;
		Ok(Self {
			hops: HopPath::new(hops),
			cost,
		})
	}

	/// Whether this advertisement looped back through `self_origin`.
	///
	/// A receiver discards such an advertisement: forwarding it would extend the loop,
	/// and subscribing through it would route the receiver back to itself. Hop ID 0
	/// identifies nothing, so a receiver that withheld its own identity detects nothing.
	pub fn loops(&self, self_origin: Origin) -> bool {
		self_origin != Origin::UNKNOWN && self.hops.hops().contains(&self_origin)
	}

	/// The route this advertisement describes, after charging the link it arrived on.
	///
	/// The addition saturates rather than wraps, so an absurd upstream value ranks last
	/// instead of overflowing to best.
	pub fn route(&self, link_cost: u64) -> crate::broadcast::Route {
		let mut route = crate::broadcast::Route::new()
			.with_hops(self.hops.hops().clone())
			.with_cost(self.cost.saturating_add(link_cost))
			.with_announce(true);
		route.advertised = self.cost;
		route
	}
}

/// What the peer declared in its SETUP.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Peer {
	/// The peer's Hop ID, or `None` when it did not negotiate the extension.
	///
	/// A peer that declared the reserved 0 negotiated the extension but withheld its
	/// identity, which decodes to `Some(Origin::UNKNOWN)`: the extension is on, but
	/// there is nothing to exclude.
	pub origin: Option<Origin>,

	/// What the peer said subscribing from it costs, or `None` when it priced nothing
	/// (meaning [`DEFAULT_COST`]). Directional: this prices what we pull from the peer,
	/// while our own declaration prices the other way, and the two need not match.
	pub cost: Option<u64>,
}

impl Peer {
	/// Whether the peer negotiated the extension.
	pub fn negotiated(&self) -> bool {
		self.origin.is_some()
	}

	/// The Hop ID to exclude when advertising to (and serving) this peer.
	/// [`Origin::UNKNOWN`] excludes nothing.
	pub fn exclude(&self) -> Origin {
		self.origin.unwrap_or(Origin::UNKNOWN)
	}
}

/// What pulling content across this link costs, added to the route cost of every
/// advertisement received over it.
///
/// RELAY_COST is directional: each endpoint declares what subscribing from *it* costs,
/// so the peer's declaration is what prices this direction. `local` overrides it, since
/// what we charge our own routing is local policy and a peer should not be able to
/// reprice our mesh unilaterally. Falls back to [`DEFAULT_COST`] when neither priced it.
pub fn link_cost(local: Option<u64>, peer: &Peer) -> u64 {
	local.or(peer.cost).unwrap_or(DEFAULT_COST)
}

/// Shared slot for the peer's cluster options, filled when its SETUP is read.
///
/// The publisher blocks on this before its first advertisement: the extension changes
/// the NAMESPACE encoding, so nothing can be sent until we know whether the peer speaks
/// it. Cheap to clone; every handle shares the same slot.
#[derive(Clone, Default)]
pub(crate) struct PeerSetup(kio::Shared<Option<Peer>>);

impl PeerSetup {
	/// Record what the peer declared. A SETUP carrying neither option records the
	/// default (not negotiated), which is what unblocks a waiter.
	///
	/// First write wins. The announce loops read this once and hold it for their
	/// lifetime while subscription serving re-reads it, so letting a later SETUP
	/// overwrite the identity would advertise under one exclusion and serve under
	/// another, which is how a routing loop gets back in.
	pub fn set(&self, peer: Peer) {
		let mut slot = self.0.lock();
		if slot.is_none() {
			*slot = Some(peer);
		}
	}

	/// Await the peer's SETUP.
	///
	/// The peer MUST send exactly one, so this resolves once that stream is read. Waits
	/// forever if it never does; the caller is a session task, cancelled when the driver
	/// drops.
	pub async fn get(&self) -> Peer {
		let slot = self
			.0
			.wait(|peer| match peer.is_some() {
				true => std::task::Poll::Ready(()),
				false => std::task::Poll::Pending,
			})
			.await;
		(*slot).expect("waited for Some")
	}
}

/// Read the cluster Setup Options out of a decoded SETUP parameter block.
pub fn peer_from_setup(params: &super::Parameters, version: Version) -> Result<Peer, DecodeError> {
	if !supported(version) {
		return Ok(Peer::default());
	}

	// RELAY_HOPS is odd, so its value is a length-prefixed byte string holding the
	// sender's Hop ID as a single varint.
	let origin = match params.get_bytes(super::ParameterBytes::RelayHops) {
		Some(mut bytes) => {
			// 0 is legal here: the peer speaks the extension but withholds its identity.
			let origin = Origin::decode(&mut bytes, version)?;
			if bytes.has_remaining() {
				return Err(DecodeError::TrailingBytes);
			}
			Some(origin)
		}
		None => None,
	};

	Ok(Peer {
		origin,
		cost: params.get_varint(super::ParameterVarInt::RelayCost),
	})
}

/// Write our cluster Setup Options into a SETUP parameter block.
///
/// `cost` is the price we put on this link, which only the dialing side declares.
pub fn peer_into_setup(params: &mut super::Parameters, self_origin: Origin, cost: Option<u64>, version: Version) {
	if !supported(version) {
		return;
	}

	let mut id = Vec::new();
	self_origin
		.encode(&mut id, version)
		.expect("a varint always fits a Vec");
	params.set_bytes(super::ParameterBytes::RelayHops, id);

	if let Some(cost) = cost {
		params.set_varint(super::ParameterVarInt::RelayCost, cost);
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	const VERSION: Version = Version::Draft19;

	fn origin(id: u64) -> Origin {
		Origin::new(id).unwrap()
	}

	fn hop_path(ids: &[u64]) -> HopPath {
		let hops = ids
			.iter()
			.map(|&id| match id {
				0 => Origin::UNKNOWN,
				id => origin(id),
			})
			.collect::<Vec<_>>();
		HopPath::new(OriginList::try_from(hops).unwrap())
	}

	fn round_trip(path: &HopPath) -> Result<HopPath, DecodeError> {
		let mut buf = BytesMut::new();
		path.param_encode(&mut buf, VERSION).unwrap();
		let mut bytes = buf.freeze();
		let decoded = HopPath::param_decode(&mut bytes, VERSION)?;
		assert!(!bytes.has_remaining(), "trailing bytes after decode");
		Ok(decoded)
	}

	#[test]
	fn code_points() {
		// The registry values are what other implementations key on; a typo here is
		// invisible against ourselves and fatal against anyone else. The parity is
		// load-bearing too: odd values are length-prefixed, even ones bare varints.
		assert_eq!(RELAY_HOPS, 0x40B55);
		assert_eq!(RELAY_COST, 0x40B56);
		assert_eq!(HOP_PATH, 0x40B57);
		assert_eq!(ROUTE_COST, 0x40B58);
		assert_eq!(RELAY_HOPS % 2, 1);
		assert_eq!(RELAY_COST % 2, 0);
		assert_eq!(HOP_PATH % 2, 1);
		assert_eq!(ROUTE_COST % 2, 0);
	}

	#[test]
	fn hop_path_round_trips() {
		let path = hop_path(&[7, 9, 11]);
		assert_eq!(round_trip(&path).unwrap(), path);
	}

	#[test]
	fn hop_path_has_no_inner_count() {
		// The entries fill the parameter length; a count would make us unreadable to
		// every other implementation. One byte of length plus one byte per small varint.
		let mut buf = BytesMut::new();
		hop_path(&[1, 2, 3]).param_encode(&mut buf, VERSION).unwrap();
		assert_eq!(buf.to_vec(), vec![0x03, 0x01, 0x02, 0x03]);
	}

	#[test]
	fn hop_path_allows_repeated_zero() {
		// 0 identifies nothing, so two unknown hops are not a loop.
		let path = hop_path(&[0, 5, 0]);
		assert_eq!(round_trip(&path).unwrap(), path);
	}

	#[test]
	fn hop_path_rejects_repeated_hop() {
		// A non-zero id appearing twice is a loop, which the receiver must reject.
		let mut buf = BytesMut::new();
		hop_path(&[4, 8, 4]).param_encode(&mut buf, VERSION).unwrap();
		let mut bytes = buf.freeze();
		assert!(matches!(
			HopPath::param_decode(&mut bytes, VERSION),
			Err(DecodeError::InvalidValue)
		));
	}

	#[test]
	fn hop_path_rejects_empty() {
		// The list always has at least one entry: the original publisher.
		let mut buf = BytesMut::new();
		HopPath::default().param_encode(&mut buf, VERSION).unwrap();
		let mut bytes = buf.freeze();
		assert!(matches!(
			HopPath::param_decode(&mut bytes, VERSION),
			Err(DecodeError::InvalidValue)
		));
	}

	#[test]
	fn hop_path_rejects_partial_entry() {
		// Entries must exactly fill Length. Chop the last byte off a multi-byte hop id
		// and shrink the length to match, so the value ends mid-varint.
		let mut value = Vec::new();
		origin(300).encode(&mut value, VERSION).unwrap();
		assert!(value.len() > 1, "300 should not fit in one byte");
		value.pop();

		let mut buf = BytesMut::new();
		value.encode(&mut buf, VERSION).unwrap();

		let mut bytes = buf.freeze();
		assert!(HopPath::param_decode(&mut bytes, VERSION).is_err());
	}

	#[test]
	fn forward_appends_self() {
		let mut hops = OriginList::new();
		hops.push(origin(1)).unwrap();
		hops.push(origin(2)).unwrap();

		let advert = Advert::forward(&hops, 5, origin(3)).unwrap();
		assert_eq!(advert.hops, hop_path(&[1, 2, 3]));
		assert_eq!(advert.hops.hops().iter().next().copied(), Some(origin(1)));
		assert_eq!(advert.cost, 5);
	}

	#[test]
	fn loops_ignores_unknown() {
		let advert = Advert {
			hops: hop_path(&[0, 5]),
			cost: 0,
		};
		// A receiver whose own id is 0 cannot detect loops through itself.
		assert!(!advert.loops(Origin::UNKNOWN));
		assert!(advert.loops(origin(5)));
		assert!(!advert.loops(origin(6)));
	}

	#[test]
	fn route_charges_the_link_and_saturates() {
		let advert = Advert {
			hops: hop_path(&[1, 2]),
			cost: 4,
		};
		let route = advert.route(3);
		assert_eq!(route.cost, 7);
		assert_eq!(&route.hops, hop_path(&[1, 2]).hops());
		assert!(route.announce);

		let absurd = Advert {
			hops: hop_path(&[1]),
			cost: u64::MAX,
		};
		assert_eq!(absurd.route(10).cost, u64::MAX);
	}

	#[test]
	fn peer_defaults() {
		let peer = Peer::default();
		assert!(!peer.negotiated());
		assert_eq!(peer.exclude(), Origin::UNKNOWN);

		// A declared 0 negotiates the extension but excludes nothing.
		let anonymous = Peer {
			origin: Some(Origin::UNKNOWN),
			cost: Some(0),
		};
		assert!(anonymous.negotiated());
		assert_eq!(anonymous.exclude(), Origin::UNKNOWN);
	}

	#[test]
	fn link_cost_prefers_local_policy_over_the_peer() {
		let unpriced = Peer::default();
		let priced = Peer {
			origin: Some(origin(9)),
			cost: Some(7),
		};
		let free = Peer {
			origin: Some(origin(9)),
			cost: Some(0),
		};

		// The peer prices its own egress, so absent local policy that is what
		// pulling from it costs, whichever side dialed.
		assert_eq!(link_cost(None, &priced), 7);
		// Zero is a price, not "unset": the peer declared this direction free.
		assert_eq!(link_cost(None, &free), 0);
		// Local policy wins: a peer cannot reprice our routing by declaring a
		// cheaper egress than we are willing to believe.
		assert_eq!(link_cost(Some(3), &priced), 3);
		assert_eq!(link_cost(Some(0), &priced), 0);
		// Nobody priced it, so this direction ranks by hop count.
		assert_eq!(link_cost(None, &unpriced), DEFAULT_COST);
	}

	#[test]
	fn setup_options_round_trip() {
		for (self_origin, cost) in [(origin(42), Some(0)), (origin(42), Some(9)), (Origin::UNKNOWN, None)] {
			let mut params = super::super::Parameters::default();
			peer_into_setup(&mut params, self_origin, cost, VERSION);

			let mut buf = BytesMut::new();
			params.encode(&mut buf, VERSION).unwrap();
			let mut bytes = buf.freeze();
			let decoded = super::super::Parameters::decode(&mut bytes, VERSION).unwrap();

			let peer = peer_from_setup(&decoded, VERSION).unwrap();
			assert_eq!(peer.origin, Some(self_origin));
			assert_eq!(peer.cost, cost);
		}
	}

	/// The announce loops read the peer's declaration once and hold it, while
	/// subscription serving re-reads it. A second SETUP overwriting the identity would
	/// split those two apart, so the first write is the one that counts.
	#[tokio::test]
	async fn peer_setup_first_write_wins() {
		let slot = PeerSetup::default();
		slot.set(Peer {
			origin: Some(origin(42)),
			cost: Some(3),
		});
		slot.set(Peer {
			origin: Some(origin(99)),
			cost: Some(0),
		});

		let peer = slot.get().await;
		assert_eq!(peer.origin, Some(origin(42)));
		assert_eq!(peer.cost, Some(3));
	}

	#[test]
	fn setup_without_relay_hops_is_not_negotiated() {
		let params = super::super::Parameters::default();
		let peer = peer_from_setup(&params, VERSION).unwrap();
		assert!(!peer.negotiated());
	}

	#[test]
	fn setup_options_skipped_before_draft17() {
		// Draft-14..16 exchange SETUP over the legacy control stream, which this
		// extension does not cover; we neither send nor read the options there.
		let mut params = super::super::Parameters::default();
		peer_into_setup(&mut params, origin(42), Some(3), Version::Draft16);
		assert!(params.get_bytes(super::super::ParameterBytes::RelayHops).is_none());
		assert!(!peer_from_setup(&params, Version::Draft16).unwrap().negotiated());
	}
}
