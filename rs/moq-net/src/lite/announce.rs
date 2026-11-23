use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Hop, Hops, Instance, Path,
	coding::*,
	origin::Cost,
	route::{
		self,
		node::{Attr, Claim},
		relay::{Hop as RouteHop, NodeId, Offer},
	},
};

use super::{
	Message, Version,
	message::{MAX_MESSAGE_SIZE, decode_size},
};

// lite-06 announce message types: an outer discriminator carried before the length
// prefix, so each announcement is an independently-typed, length-delimited message
// (mirroring SUBSCRIBE_START/END/DROP on the subscribe stream).
const ANNOUNCE_START: u64 = 0;
const ANNOUNCE_END: u64 = 1;
const ANNOUNCE_UPDATE: u64 = 2;
// lite-07: one claims message per change on a routing neighbour's stream is closed by a floor.
const ANNOUNCE_FLOOR: u64 = 3;
const SYNCED: u64 = 4;
// lite-07: an edit of the prefixes the sender wants, and the marker answering one.
const ANNOUNCE_REQUEST: u64 = 5;
const ANNOUNCE_OK: u64 = 6;
// lite-07: the routes between forwarding nodes.
const ROUTE: u64 = 7;
const ROUTE_END: u64 = 8;
const DEAD: u64 = 9;
const DRAIN: u64 = 10;

/// Whether the negotiated version can update a live advertisement's metadata. On lite-05 an
/// update travels as a duplicate ANNOUNCE (a second `active` for an already-announced path); on
/// lite-06+ it is ANNOUNCE_UPDATE referencing an announce id. Older versions never defined this,
/// so we neither send nor interpret it there; their peers keep the hop chain from the original
/// announce.
pub fn update_supported(version: Version) -> bool {
	// Explicitly list older versions so future versions default to supported.
	!matches!(
		version,
		Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04
	)
}

/// A message on the Announce Stream, advertising or retracting a broadcast.
///
/// Lite-07 carries everything one side says about routing on its one unidirectional
/// stream: the edits of the prefixes it wants (`Request`), and the markers (`Ok`,
/// `Synced`), routes, and claims answering the prefixes its peer wants.
///
/// On lite-06+ these are independently-typed messages, each framed as
/// `Type | Length | Body` like the subscribe stream's responses. Each ANNOUNCE_START
/// implicitly assigns the next announce id (a per-stream ordinal starting at 0); the
/// others reference that id instead of repeating the path. Older versions send a single
/// `ANNOUNCE_BROADCAST` message that retracts by path (`Ended`).
///
/// The path is as it appears on the wire: on lite-07 it may name a base announcement,
/// resolved against the stream's history by [`AnnounceDecoder`](super::AnnounceDecoder).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnounceBroadcast {
	/// ANNOUNCE_START (lite-06) / active (older): a broadcast is now available.
	/// Carries the path suffix, the hop chain, and (lite-06) the cost, and assigns the
	/// next announce id.
	Active { suffix: PathRef, hops: Hops, cost: Cost },
	/// Pre-lite-06: a broadcast is no longer available, retracted by path.
	Ended { suffix: Path, hops: Hops },
	/// ANNOUNCE_END (lite-06): a broadcast is no longer available, retracted by
	/// announce id. The id is retired; referencing it again is a protocol violation.
	EndedId { id: u64 },
	/// ANNOUNCE_UPDATE (lite-06): atomically replace the hop chain and cost of the
	/// announcement with this id. The id stays live.
	Update { id: u64, hops: Hops, cost: Cost },
	/// ANNOUNCE_START (lite-07): a claim of the broadcast at `suffix` serving
	/// `instance`, which assigns it the next announce id.
	Start {
		suffix: PathRef,
		instance: Instance,
		claim: Claim,
	},
	/// ANNOUNCE_UPDATE (lite-07): the claim with this id now stands as `claim`.
	Change { id: u64, claim: Claim },
	/// ANNOUNCE_END (lite-07): the claim with this id is gone, and the id retired. Its
	/// publisher withdrew it unless `lost`, when the sender only lost its way to it.
	End { id: u64, lost: bool },
	/// An unknown lite-06+ announce type. The length-prefixed body was skipped so
	/// the stream stays up; it does not assign an announce id.
	Skipped,
	/// SYNCED (lite-07+): the initial set of the prefix the receiver assigned this Prefix
	/// ID has been delivered.
	Synced { prefix: u64 },
	/// ANNOUNCE_REQUEST (lite-07+): the sender edits the prefixes it wants claims for,
	/// removing those with the Prefix IDs in `remove` and then adding each of `add`,
	/// with whether it also wants hidden claims there, which assigns it the next Prefix
	/// ID. With `more`, the edit continues in the next message, and takes effect whole
	/// with its last (see [`request_parts`]).
	Request {
		more: bool,
		remove: Vec<u64>,
		add: Vec<(Path, bool)>,
	},
	/// ANNOUNCE_OK (lite-07+): the receiver's oldest unanswered edit takes effect here.
	Ok,
	/// ROUTE (lite-07+): the sender's route to the forwarding node its path begins
	/// with, replacing the one it sent before, or else assigned the next Route ID.
	Route { offer: Offer },
	/// ROUTE_END (lite-07+): the sender let go of the route with this id, and of every
	/// claim under it.
	RouteEnd { id: u64 },
	/// DEAD (lite-07+): the session `hop` names closed, at the node `far` at its other
	/// end, which ends every route the sender offered through it.
	Dead { hop: RouteHop, far: NodeId },
	/// DRAIN (lite-07+): `node` sent GOAWAY, so every route through it ranks last.
	Drain { node: NodeId },
	/// ANNOUNCE_FLOOR (lite-07+): closes the batch of claim messages since the last
	/// one, which change the claims of the node the route with this id leads to, and
	/// are complete as of that node's `revision`.
	Floor { id: u64, revision: route::Rev },
}

/// Route flag: a marked link is on the path, so it is never sent over another.
const MARKED: u8 = 0x1;
/// Route and claim flag: a node on the way sent GOAWAY.
const DRAINING: u8 = 0x2;
/// Claim flag: no route reaches the claim from the node that told it.
const UNAVAILABLE: u8 = 0x1;
/// Claim flag: a marked link lies on the way to the claim, so it is never told over another.
const CLAIM_MARKED: u8 = 0x4;

/// Encode a Node ID: its relay, start, and Marked. No node names itself 0.
pub(super) fn encode_node(w: &mut Encoder<'_>, node: NodeId) -> Result<(), EncodeError> {
	if node.relay == 0 {
		return Err(EncodeError::InvalidState);
	}
	w.varint(node.relay)?;
	w.varint(node.start)?;
	w.bool(node.marked);
	Ok(())
}

/// Decode a Node ID. A relay of 0, or a Marked other than 0 or 1, fails the stream.
pub(super) fn decode_node(r: &mut Decoder<'_>) -> Result<NodeId, DecodeError> {
	let relay = match r.varint()? {
		0 => return Err(DecodeError::InvalidValue),
		relay => relay,
	};
	let start = r.varint()?;
	let marked = r.bool()?;
	Ok(NodeId { relay, start, marked })
}

/// Decode the Node ID of a node that holds sessions, which is never a marked one.
fn decode_session_node(r: &mut Decoder<'_>) -> Result<NodeId, DecodeError> {
	match decode_node(r)? {
		node if node.marked => Err(DecodeError::InvalidValue),
		node => Ok(node),
	}
}

fn encode_hop(w: &mut Encoder<'_>, hop: RouteHop) -> Result<(), EncodeError> {
	encode_node(w, hop.node)?;
	w.varint(hop.session)
}

fn decode_hop(r: &mut Decoder<'_>) -> Result<RouteHop, DecodeError> {
	Ok(RouteHop {
		node: decode_node(r)?,
		session: r.varint()?,
	})
}

/// Encode a route: the path, then flags, priority, and cost. No node offers an empty
/// path or one past the hop limit.
fn encode_offer(w: &mut Encoder<'_>, offer: &Offer) -> Result<(), EncodeError> {
	if offer.path.is_empty() || offer.path.len() > route::MAX_HOPS {
		return Err(EncodeError::InvalidState);
	}
	w.varint(offer.path.len() as u64)?;
	for hop in offer.path.iter() {
		encode_hop(w, *hop)?;
	}
	let mut flags = 0;
	if offer.marked {
		flags |= MARKED;
	}
	if offer.draining {
		flags |= DRAINING;
	}
	w.u8(flags);
	w.u8(super::priority::to_wire(offer.priority));
	w.varint(offer.cost)
}

/// Decode a route. A sender within the limit never sends an empty path, one past the
/// hop limit, or a flag this version does not define, so each fails the stream.
fn decode_offer(r: &mut Decoder<'_>) -> Result<Offer, DecodeError> {
	let count = r.varint()?;
	if count == 0 {
		return Err(DecodeError::InvalidValue);
	}
	if count > route::MAX_HOPS as u64 {
		return Err(DecodeError::TooMany);
	}
	let path = (0..count).map(|_| decode_hop(r)).collect::<Result<Vec<_>, _>>()?;
	// Only the node a route leads to may be a marked identity.
	if path.iter().skip(1).any(|hop| hop.node.marked) {
		return Err(DecodeError::InvalidValue);
	}
	let flags = r.u8()?;
	if flags & !(MARKED | DRAINING) != 0 {
		return Err(DecodeError::InvalidValue);
	}
	let priority = super::priority::from_wire(r.u8()?);
	let cost = r.varint()?;
	Ok(Offer {
		path: path.into(),
		draining: flags & DRAINING != 0,
		priority,
		cost,
		marked: flags & MARKED != 0,
	})
}

/// Encode what a claim says after its epoch: cost, above, flags, priority, and chain.
fn encode_claim(w: &mut Encoder<'_>, claim: &Claim) -> Result<(), EncodeError> {
	if claim.chain.len() >= route::MAX_HOPS || claim.chain.contains(&0) {
		return Err(EncodeError::InvalidState);
	}
	w.varint(claim.cost)?;
	w.varint(claim.item.above)?;
	let mut flags = 0;
	if claim.unavailable {
		flags |= UNAVAILABLE;
	}
	if claim.draining {
		flags |= DRAINING;
	}
	if claim.marked {
		flags |= CLAIM_MARKED;
	}
	w.u8(flags);
	w.u8(super::priority::to_wire(claim.priority));
	w.varint(claim.chain.len() as u64)?;
	for relay in &claim.chain {
		w.varint(*relay)?;
	}
	Ok(())
}

/// Decode a claim of `epoch`. A chain at the hop limit, or naming Node ID 0, and a
/// flag this version does not define fail the stream: no sender builds one.
fn decode_claim(r: &mut Decoder<'_>, epoch: u64) -> Result<Claim, DecodeError> {
	// A cost saturates at the ceiling, so one above it reads as the ceiling and is never
	// forwarded raw.
	let cost = r.varint()?.min(route::MAX_COST);
	let above = r.varint()?;
	let flags = r.u8()?;
	if flags & !(UNAVAILABLE | DRAINING | CLAIM_MARKED) != 0 {
		return Err(DecodeError::InvalidValue);
	}
	let priority = super::priority::from_wire(r.u8()?);
	let count = r.varint()?;
	// A chain reaches the hop limit only with the Relay of a node that then does not take
	// the claim, so none that long is sent.
	if count >= route::MAX_HOPS as u64 {
		return Err(DecodeError::TooMany);
	}
	let mut chain = Vec::with_capacity(count as usize);
	for _ in 0..count {
		match r.varint()? {
			0 => return Err(DecodeError::InvalidValue),
			relay => chain.push(relay),
		}
	}
	Ok(Claim {
		item: Attr { epoch, above },
		cost,
		priority,
		draining: flags & DRAINING != 0,
		unavailable: flags & UNAVAILABLE != 0,
		chain,
		marked: flags & CLAIM_MARKED != 0,
	})
}

/// Encode an ANNOUNCE_REQUEST body: the Prefix IDs to remove, then the prefixes to add,
/// each with its `Hidden`.
fn encode_request(
	w: &mut Encoder<'_>,
	version: Version,
	more: bool,
	remove: &[u64],
	add: &[(Path, bool)],
) -> Result<(), EncodeError> {
	w.bool(more);
	w.varint(remove.len() as u64)?;
	for id in remove {
		w.varint(*id)?;
	}
	w.varint(add.len() as u64)?;
	for (prefix, hidden) in add {
		prefix.encode(w, version)?;
		w.bool(*hidden);
	}
	Ok(())
}

/// The bytes `value` takes as a varint of `version`.
fn varint_size(value: u64, version: Version) -> usize {
	crate::coding::varint::size(value, version.into()).unwrap_or(usize::MAX)
}

/// The bytes one Add of `prefix` takes: its length, its bytes, and its flag.
fn add_size(prefix: &Path, version: Version) -> usize {
	let len = prefix.as_str().len();
	varint_size(len as u64, version).saturating_add(len + 1)
}

/// The bytes an ANNOUNCE_REQUEST body takes with `removes` Prefix IDs totalling `ids`
/// bytes and `adds` Adds totalling `added` bytes.
fn request_size(removes: usize, ids: usize, adds: usize, added: usize, version: Version) -> usize {
	(1 + varint_size(removes as u64, version) + varint_size(adds as u64, version))
		.saturating_add(ids)
		.saturating_add(added)
}

/// Whether the prefixes `set` fit the Adds of one ANNOUNCE_REQUEST, as a subscriber's
/// whole set must.
pub(crate) fn request_fits<'a>(set: impl Iterator<Item = &'a Path>, version: Version) -> bool {
	let mut count = 0;
	let mut bytes = 0usize;
	for prefix in set {
		count += 1;
		bytes = bytes.saturating_add(add_size(prefix, version));
	}
	request_size(0, 0, count, bytes, version) <= super::message::MAX_MESSAGE_SIZE
}

/// The ANNOUNCE_REQUESTs of one edit removing the Prefix IDs `remove` and adding `add`:
/// as many as it takes for each to fit one message, every one but the last with `more`.
/// The receiver applies them together at the last, so how they split changes nothing.
pub(crate) fn request_parts(remove: Vec<u64>, add: Vec<(Path, bool)>, version: Version) -> Vec<AnnounceBroadcast> {
	let mut parts = Vec::new();
	let mut part = (Vec::new(), Vec::new());
	// The removes' count and bytes, then the adds'.
	let mut size = (0, 0);
	let fits = |part: &(Vec<u64>, Vec<(Path, bool)>), size: (usize, usize), more: (usize, usize)| {
		let (removes, adds) = (
			part.0.len() + usize::from(more.0 > 0),
			part.1.len() + usize::from(more.1 > 0),
		);
		request_size(removes, size.0 + more.0, adds, size.1 + more.1, version) <= super::message::MAX_MESSAGE_SIZE
	};
	for id in remove {
		let bytes = varint_size(id, version);
		if !fits(&part, size, (bytes, 0)) {
			parts.push(std::mem::take(&mut part));
			size = (0, 0);
		}
		part.0.push(id);
		size.0 += bytes;
	}
	for (prefix, hidden) in add {
		let bytes = add_size(&prefix, version);
		if !fits(&part, size, (0, bytes)) {
			parts.push(std::mem::take(&mut part));
			size = (0, 0);
		}
		part.1.push((prefix, hidden));
		size.1 += bytes;
	}
	parts.push(part);
	let last = parts.len() - 1;
	(parts.into_iter().enumerate())
		.map(|(i, (remove, add))| AnnounceBroadcast::Request {
			more: i < last,
			remove,
			add,
		})
		.collect()
}

/// Decode an ANNOUNCE_REQUEST body. The prefixes are the sender's own, so one that is
/// no path fails the message rather than being refused.
fn decode_request(r: &mut Decoder<'_>, version: Version) -> Result<AnnounceBroadcast, DecodeError> {
	let more = r.bool()?;
	let count = r.varint()?;
	// Each id takes at least a byte, so the body bounds the count.
	if count > r.remaining() as u64 {
		return Err(DecodeError::TooMany);
	}
	let remove = (0..count).map(|_| r.varint()).collect::<Result<_, _>>()?;
	let count = r.varint()?;
	// Each prefix takes at least its length and its flag.
	if count > (r.remaining() / 2) as u64 {
		return Err(DecodeError::TooMany);
	}
	let add = (0..count)
		.map(|_| Ok((Path::decode(r, version)?, r.bool()?)))
		.collect::<Result<_, DecodeError>>()?;
	Ok(AnnounceBroadcast::Request { more, remove, add })
}

/// A path suffix as it travels: the first `keep` segments of a base announcement's
/// suffix, followed by `rest`.
///
/// `base` is the base's distance back from the stream's next unassigned announce id,
/// so 1 is the latest ANNOUNCE_START, and 0 names no base (`keep` must be 0 too). Only
/// lite-07 carries a base; every other version is `rest` alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRef {
	pub base: u64,
	pub keep: u64,
	pub rest: Path,
}

impl PathRef {
	/// The whole suffix, with no base.
	pub fn literal(rest: Path) -> Self {
		Self { base: 0, keep: 0, rest }
	}
}

impl Encode<Version> for PathRef {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if version.has_announce_compression() {
			w.varint(self.base)?;
			w.varint(self.keep)?;
		} else if self.base != 0 || self.keep != 0 {
			return Err(EncodeError::Version);
		}
		self.rest.encode(w, version)
	}
}

impl<'a> Decode<'a, Version> for PathRef {
	fn decode(buf: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_announce_compression() {
			return Ok(Self::literal(Path::decode(buf, version)?));
		}
		let base = buf.varint()?;
		let keep = buf.varint()?;
		if base == 0 && keep != 0 {
			return Err(DecodeError::InvalidValue);
		}
		let rest = Path::decode(buf, version)?;
		Ok(Self { base, keep, rest })
	}
}

impl Encode<Version> for Cost {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_route_cost() {
			return Ok(());
		}
		w.varint(self.value())?;
		if version == Version::Lite06 {
			// lite-06 also carries Cold; write the ceiling so it ranks last on old readers.
			w.varint(Cost::MAX.value())?;
		}
		Ok(())
	}
}

impl<'a> Decode<'a, Version> for Cost {
	fn decode(buf: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_route_cost() {
			return Ok(Cost::UNKNOWN);
		}
		// Costs saturate at 2^62-1 on every version, so a larger one (lite-07's varints
		// reach 2^64-1) reads as the ceiling and still forwards to an older peer.
		let cost = Cost::new(buf.varint()?);
		if version == Version::Lite06 {
			// lite-06's Cold has no selection rule; read and ignore it.
			buf.varint()?;
		}
		Ok(cost)
	}
}

impl Encode<Version> for AnnounceBroadcast {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Message::Announce);
		if version.has_announce_id() {
			// Lite06+: outer type discriminator, then a size-prefixed body (like the
			// subscribe stream). The body varies by type.
			let routes = version.has_routes();
			let typ = match self {
				// Each version has its own form of a claim.
				Self::Active { .. } if !routes => ANNOUNCE_START,
				Self::EndedId { .. } if !routes => ANNOUNCE_END,
				Self::Update { .. } if !routes => ANNOUNCE_UPDATE,
				Self::Start { .. } if routes => ANNOUNCE_START,
				Self::End { .. } if routes => ANNOUNCE_END,
				Self::Change { .. } if routes => ANNOUNCE_UPDATE,
				Self::Synced { .. } if routes => SYNCED,
				Self::Request { .. } if routes => ANNOUNCE_REQUEST,
				Self::Ok if routes => ANNOUNCE_OK,
				Self::Route { .. } if routes => ROUTE,
				Self::RouteEnd { .. } if routes => ROUTE_END,
				Self::Dead { .. } if routes => DEAD,
				Self::Drain { .. } if routes => DRAIN,
				Self::Floor { .. } if routes => ANNOUNCE_FLOOR,
				// Decode-only: an unknown type is never sent.
				Self::Skipped => return Err(EncodeError::Unsupported),
				// The pre-lite-06 path-form retraction has no place on lite-06.
				_ => return Err(EncodeError::Version),
			};
			w.varint(typ)?;

			let prefix = w.prefix_varint();
			match self {
				Self::Active { suffix, hops, cost } => {
					suffix.encode(w, version)?;
					hops.encode(w, version)?;
					cost.encode(w, version)?;
				}
				Self::EndedId { id } => w.varint(*id)?,
				Self::Update { id, hops, cost } => {
					w.varint(*id)?;
					hops.encode(w, version)?;
					cost.encode(w, version)?;
				}
				Self::Start {
					suffix,
					instance,
					claim,
				} => {
					suffix.encode(w, version)?;
					super::instance::encode_route(w, version, Some(instance), claim.item.epoch)?;
					encode_claim(w, claim)?;
				}
				Self::Change { id, claim } => {
					w.varint(*id)?;
					w.varint(claim.item.epoch)?;
					encode_claim(w, claim)?;
				}
				Self::End { id, lost } => {
					w.varint(*id)?;
					w.bool(*lost);
				}
				Self::Synced { prefix } => w.varint(*prefix)?,
				Self::Request { more, remove, add } => encode_request(w, version, *more, remove, add)?,
				Self::Ok => {}
				Self::Route { offer } => encode_offer(w, offer)?,
				Self::RouteEnd { id } => w.varint(*id)?,
				Self::Dead { hop, far } => {
					encode_hop(w, *hop)?;
					encode_node(w, *far)?;
				}
				Self::Drain { node } => encode_node(w, *node)?,
				Self::Floor { id, revision } => {
					w.varint(*id)?;
					w.varint(*revision)?;
				}
				Self::Ended { .. } | Self::Skipped => unreachable!("refused above"),
			}
			if w.since(&prefix) > MAX_MESSAGE_SIZE {
				w.discard(prefix);
				return Err(EncodeError::TooLarge);
			}
			return w.fill(prefix);
		}

		// Older versions: a single ANNOUNCE_BROADCAST message, size-prefixed, with the
		// status carried inside the body.
		let prefix = w.prefix_varint();
		match self {
			// The cost is a lite-06 addition, so it is simply not on the wire here.
			Self::Active { suffix, hops, .. } => {
				// Bases are a lite-07 addition; PathRef refuses one here.
				if suffix.base != 0 {
					return Err(EncodeError::Version);
				}
				AnnounceStatus::Active.encode(w, version)?;
				suffix.rest.encode(w, version)?;
				encode_hops(w, version, hops)?;
			}
			Self::Ended { suffix, hops } => {
				AnnounceStatus::Ended.encode(w, version)?;
				suffix.encode(w, version)?;
				encode_hops(w, version, hops)?;
			}
			// Everything else only exists on lite-06+.
			_ => return Err(EncodeError::Version),
		}
		if w.since(&prefix) > MAX_MESSAGE_SIZE {
			w.discard(prefix);
			return Err(EncodeError::TooLarge);
		}
		w.fill(prefix)
	}
}

impl<'a> Decode<'a, Version> for AnnounceBroadcast {
	fn decode(buf: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		buf.control = Some(crate::stats::Message::Announce);
		if version.has_announce_id() {
			// Lite06+: outer type, then a size-prefixed body decoded within its bounds.
			let typ = buf.varint()?;
			let size = decode_size(buf, MAX_MESSAGE_SIZE)?;
			let mut body = buf.sub(size)?;
			let msg = (|| {
				Ok(match typ {
					ANNOUNCE_START if version.has_routes() => {
						let suffix = PathRef::decode(&mut body, version)?;
						// Every lite-07 claim names its instance.
						let (instance, epoch) = super::instance::decode_forwarded(&mut body)?;
						let claim = decode_claim(&mut body, epoch)?;
						Self::Start {
							suffix,
							instance,
							claim,
						}
					}
					ANNOUNCE_UPDATE if version.has_routes() => {
						let id = body.varint()?;
						let epoch = body.varint()?;
						Self::Change {
							id,
							claim: decode_claim(&mut body, epoch)?,
						}
					}
					ANNOUNCE_END if version.has_routes() => Self::End {
						id: body.varint()?,
						lost: body.bool()?,
					},
					ANNOUNCE_START => {
						let suffix = PathRef::decode(&mut body, version)?;
						let hops = Hops::decode(&mut body, version)?;
						let cost = Cost::decode(&mut body, version)?;
						Self::Active { suffix, hops, cost }
					}
					ANNOUNCE_END => Self::EndedId { id: body.varint()? },
					ANNOUNCE_UPDATE => {
						let id = body.varint()?;
						let hops = Hops::decode(&mut body, version)?;
						let cost = Cost::decode(&mut body, version)?;
						Self::Update { id, hops, cost }
					}
					SYNCED if version.has_routes() => Self::Synced { prefix: body.varint()? },
					ANNOUNCE_REQUEST if version.has_routes() => decode_request(&mut body, version)?,
					ANNOUNCE_OK if version.has_routes() => Self::Ok,
					ROUTE if version.has_routes() => Self::Route {
						offer: decode_offer(&mut body)?,
					},
					ROUTE_END if version.has_routes() => Self::RouteEnd { id: body.varint()? },
					// A marked Node ID holds no session, so it neither loses one nor drains.
					DEAD if version.has_routes() => Self::Dead {
						hop: RouteHop {
							node: decode_session_node(&mut body)?,
							session: body.varint()?,
						},
						far: decode_session_node(&mut body)?,
					},
					DRAIN if version.has_routes() => Self::Drain {
						node: decode_session_node(&mut body)?,
					},
					ANNOUNCE_FLOOR if version.has_routes() => Self::Floor {
						id: body.varint()?,
						revision: body.varint()?,
					},
					// Unknown types are skipped by length so an earlier Lite06 build
					// negotiating the same ALPN does not kill the announce stream.
					_ => {
						body.rest();
						Self::Skipped
					}
				})
			})()
			.map_err(DecodeError::complete)?;
			if !body.is_empty() {
				return Err(DecodeError::Long);
			}
			return Ok(msg);
		}

		// Older versions: a single size-prefixed ANNOUNCE_BROADCAST with an inner status.
		let size = decode_size(buf, MAX_MESSAGE_SIZE)?;
		let mut body = buf.sub(size)?;
		let msg = Self::decode_legacy(&mut body, version).map_err(DecodeError::complete)?;
		if !body.is_empty() {
			return Err(DecodeError::Long);
		}
		Ok(msg)
	}
}

impl AnnounceBroadcast {
	/// Decode the body of a pre-lite-06 ANNOUNCE_BROADCAST (inner status + path + hops).
	fn decode_legacy(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let status = AnnounceStatus::decode(r, version)?;

		let suffix = Path::decode(r, version)?;
		let hops = match version {
			Version::Lite01 | Version::Lite02 => Hops::new(),
			Version::Lite03 => {
				// Lite03 sends only a hop count, not individual ids. Fill with UNKNOWN placeholders.
				// push() enforces MAX_HOPS and `?` lifts the overflow to DecodeError::BoundsExceeded.
				let count = r.varint()? as usize;
				let mut list = Hops::new();
				for _ in 0..count {
					list.push(Hop::UNKNOWN)?;
				}
				list
			}
			_ => Hops::decode(r, version)?,
		};

		Ok(match status {
			AnnounceStatus::Active => Self::Active {
				suffix: PathRef::literal(suffix),
				hops,
				cost: Cost::UNKNOWN,
			},
			AnnounceStatus::Ended => Self::Ended { suffix, hops },
			// On lite-05 an update travels as a duplicate ANNOUNCE (a second `Active`), so accept
			// the explicit `restart` status the lite-05 draft named and treat it the same. Either
			// way the subscriber re-prices an already-announced path in place; for an unknown
			// path it's a fresh announce. Older versions never defined this status, so it's an
			// invalid value there.
			AnnounceStatus::Update if update_supported(version) => Self::Active {
				suffix: PathRef::literal(suffix),
				hops,
				cost: Cost::UNKNOWN,
			},
			AnnounceStatus::Update => return Err(DecodeError::InvalidValue),
		})
	}
}

fn encode_hops(w: &mut Encoder<'_>, version: Version, hops: &Hops) -> Result<(), EncodeError> {
	match version {
		Version::Lite01 | Version::Lite02 => Ok(()),
		Version::Lite03 => {
			w.varint(hops.len() as u64)?;
			Ok(())
		}
		_ => hops.encode(w, version),
	}
}

/// ANNOUNCE_REQUEST before lite-07: sent by the subscriber to request
/// ANNOUNCE_BROADCAST messages for a path prefix, opening a bidirectional Announce
/// Stream. Renamed from ANNOUNCE_INTEREST in lite-05. Lite-07 edits a set of prefixes on
/// its one stream instead ([`AnnounceBroadcast::Request`]).
#[derive(Clone, Debug)]
pub struct AnnounceRequest {
	// Request tracks with this prefix.
	pub prefix: Path,
	// Lite04/05 only: unless unknown, the publisher SHOULD skip announces whose hop IDs
	// contain this value. Not on the wire elsewhere, so the value set here is ignored
	// when encoding for another version and decodes as unknown; lite-06 carries the
	// identity session-wide in the SETUP Hop parameter instead.
	pub exclude_hop: Hop,
}

impl<'a> Message<'a> for AnnounceRequest {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Announce);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if version.has_routes() {
			return Err(DecodeError::Version);
		}
		let prefix = Path::decode(r, version)?;
		let exclude_hop = match version.has_exclude_hop() {
			true => Hop::decode(r, version)?,
			false => Hop::UNKNOWN,
		};
		Ok(Self { prefix, exclude_hop })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if version.has_routes() {
			return Err(EncodeError::Version);
		}
		self.prefix.encode(w, version)?;
		if version.has_exclude_hop() {
			self.exclude_hop.encode(w, version)?;
		}

		Ok(())
	}
}

/// Send by the publisher, used to determine the message that follows.
#[derive(Clone, Copy, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum AnnounceStatus {
	Ended = 0,
	Active = 1,
	/// The explicit update status (`restart` in the lite-05 draft), accepted on decode for
	/// forward/cross-compatibility. We never encode it: a lite-05 update goes out as a
	/// duplicate `Active`.
	Update = 2,
}

impl<'a> Decode<'a, Version> for AnnounceStatus {
	fn decode(r: &mut Decoder<'a>, _: Version) -> Result<Self, DecodeError> {
		r.control = Some(crate::stats::Message::Announce);
		let status = r.u8()?;
		status.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for AnnounceStatus {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Message::Announce);
		w.u8(*self as u8);
		Ok(())
	}
}

/// Sent after setup to communicate the initially announced paths.
///
/// Used by Draft01/Draft02 only. Draft03 uses individual Announce messages instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnounceInit {
	/// List of currently active broadcasts, encoded as suffixes to be combined with the prefix.
	pub suffixes: Vec<Path>,
}

impl AnnounceInit {
	/// Decode one size-prefixed ANNOUNCE_INIT, refusing one that lists more than `budget`
	/// suffixes before decoding any, so the set grows no larger than what may be announced.
	pub(crate) fn decode_within(r: &mut Decoder<'_>, version: Version, budget: usize) -> Result<Self, DecodeError> {
		super::message::decode_with(r, |body| Self::decode_suffixes(body, version, budget))
	}

	fn decode_suffixes(r: &mut Decoder<'_>, version: Version, budget: usize) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				return Err(DecodeError::Version);
			}
		}

		let count = r.varint()?;
		if count > budget as u64 {
			return Err(DecodeError::TooMany);
		}
		// Each suffix takes at least its length byte, so the body bounds the count too.
		if count > r.remaining() as u64 {
			return Err(DecodeError::Short);
		}

		let mut paths = Vec::with_capacity(count as usize);
		for _ in 0..count {
			paths.push(Path::decode(r, version)?);
		}

		Ok(Self { suffixes: paths })
	}
}

impl<'a> Message<'a> for AnnounceInit {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Announce);
	// Published lite01/02 carry the whole initial set in one message. Capped so it fits one
	// storage segment with its type and size prefixes (a varint of at most 8 bytes each), which
	// keeps the smallest receive window a leasing transport may offer at one segment. A larger
	// set is refused on both ends rather than raising every session's window for it.
	const MAX_SIZE: usize = crate::frame::SEGMENT - 16;

	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		Self::decode_suffixes(r, version, usize::MAX)
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				return Err(EncodeError::Version);
			}
		}

		w.varint(self.suffixes.len() as u64)?;
		for path in &self.suffixes {
			path.encode(w, version)?;
		}

		Ok(())
	}
}

/// Sent by the publisher as the first message on an announce stream, before any
/// individual Announce messages. Lite05+ only; the successor to [`AnnounceInit`].
///
/// `origin` is the responder's session origin id. In Lite05 the publisher no
/// longer stamps it onto each Announce's hop chain; the subscriber appends it on
/// receipt instead. Lite05/06 carry it and `active`, the number of starts immediately
/// following. Lite07 answers on its one stream instead ([`AnnounceBroadcast::Ok`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnounceOk {
	pub origin: Hop,
	pub active: u64,
}

impl<'a> Message<'a> for AnnounceOk {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Announce);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_announce_ok() || version.has_routes() {
			return Err(DecodeError::Version);
		}

		let origin = Hop::decode(r, version)?;
		let active = r.varint()?;
		Ok(Self { origin, active })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_announce_ok() || version.has_routes() {
			return Err(EncodeError::Version);
		}

		self.origin.encode(w, version)?;
		w.varint(self.active)
	}
}

#[cfg(all(test, feature = "async"))]
mod chain_tests {
	use super::*;

	/// A claim's chain names at most 31 relays: one more would be the Relay of a node that
	/// does not take the claim, so the codec refuses it in both directions.
	#[test]
	fn a_claim_chain_at_the_hop_limit_is_refused() {
		let form = Version::Lite07.into();
		let claim = |relays: u64| Claim {
			item: route::node::Attr { epoch: 0, above: 0 },
			cost: 0,
			priority: i8::MAX,
			draining: false,
			unavailable: false,
			chain: (1..=relays).collect(),
			marked: false,
		};
		let encode = |relays: u64| {
			let mut buf = Vec::new();
			encode_claim(&mut Encoder::new(&mut buf, form), &claim(relays)).map(|()| buf)
		};
		let longest = route::MAX_HOPS as u64 - 1;

		let buf = encode(longest).expect("the longest chain");
		let decoded = decode_claim(&mut Decoder::new(&buf, form), 0).expect("the longest chain");
		assert_eq!(decoded.chain.len() as u64, longest);
		assert!(encode(longest + 1).is_err());

		// One relay more, as only a peer outside the rule sends: every field is one byte.
		let mut buf = vec![0, 0, 0, 0, route::MAX_HOPS as u8];
		buf.extend(1..=route::MAX_HOPS as u8);
		let refused = decode_claim(&mut Decoder::new(&buf, form), 0);
		assert!(matches!(refused, Err(DecodeError::TooMany)), "{refused:?}");
	}

	/// A cost above the draft's ceiling, which lite-07's varint can still carry, reads as
	/// the ceiling, so this node never forwards it raw.
	#[test]
	fn a_claim_cost_saturates_at_the_ceiling() {
		let form = Version::Lite07.into();
		for (sent, read) in [
			(route::MAX_COST, route::MAX_COST),
			(route::MAX_COST + 1, route::MAX_COST),
			(u64::MAX, route::MAX_COST),
		] {
			let claim = Claim {
				item: route::node::Attr { epoch: 0, above: 0 },
				cost: sent,
				priority: i8::MAX,
				draining: false,
				unavailable: false,
				chain: Vec::new(),
				marked: false,
			};
			let mut buf = Vec::new();
			encode_claim(&mut Encoder::new(&mut buf, form), &claim).unwrap();
			let decoded = decode_claim(&mut Decoder::new(&buf, form), 0).unwrap();
			assert_eq!(decoded.cost, read, "{sent}");
		}
	}
}
