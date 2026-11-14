use std::sync::Arc;

use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Hop, Hops, Instance, Path,
	coding::*,
	origin::Cost,
	route::{self, Advert},
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
// lite-07: the advertisement serves another instance, or a newer epoch of its own.
const ANNOUNCE_RESTART: u64 = 3;
const SYNCED: u64 = 4;
// lite-07: the route layer. ROUTE_END also travels back as the echo.
const ROUTE_START: u64 = 6;
const ROUTE_UPDATE: u64 = 7;
const ROUTE_END: u64 = 8;
const ANNOUNCE_FLOOR: u64 = 9;
const ROUTE_REQUEST: u64 = 10;
const ROUTE_HOLD: u64 = 11;
// lite-07: warm routes, one hop toward the subscriber.
const WARM_START: u64 = 12;
const WARM_UPDATE: u64 = 13;
const WARM_END: u64 = 14;

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

/// An announcement on the Announce Stream, advertising or retracting a broadcast.
///
/// On lite-06+ these are independently-typed messages (`ANNOUNCE_START`,
/// `ANNOUNCE_END`, `ANNOUNCE_UPDATE`, and on lite-07 `ANNOUNCE_RESTART`), each
/// framed as `Type | Length | Body` like the subscribe stream's responses. Each
/// `Active` (ANNOUNCE_START) implicitly assigns the next announce id (a
/// per-stream ordinal starting at 0); the others reference that id instead of
/// repeating the path. Older versions send a single `ANNOUNCE_BROADCAST`
/// message that retracts by path (`Ended`).
///
/// The path is as it appears on the wire: on lite-07 it may name a base announcement,
/// resolved against the stream's history by [`AnnounceDecoder`](super::AnnounceDecoder).
///
/// Lite-07 names the route each claim travels under in place of a hop chain, and `route`
/// must be 0 on every earlier version. There `hops` is the Lite-06 Chain instead: the
/// chain of a claim learned over moq-lite-06, empty for every other claim, and sent only
/// to a peer that offered the Lite-06 Chain Setup Parameter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnounceBroadcast {
	/// ANNOUNCE_START (lite-06) / active (older): a broadcast is now available.
	/// Carries the path suffix, (lite-07+) the Route ID and the instance and epoch it
	/// serves, (before lite-07) the hop chain, and (lite-06+) the cost, and assigns the
	/// next announce id. The instance and epoch change only with a `Restart`.
	Active {
		suffix: PathRef,
		route: u64,
		instance: Option<Instance>,
		epoch: u64,
		hops: Hops,
		cost: Cost,
	},
	/// Pre-lite-06: a broadcast is no longer available, retracted by path.
	Ended { suffix: Path, hops: Hops },
	/// ANNOUNCE_END (lite-06+): a broadcast is no longer available, retracted by
	/// announce id. The id is retired; referencing it again is a protocol violation.
	EndedId { id: u64 },
	/// ANNOUNCE_UPDATE (lite-06+): atomically replace the metadata of the announcement
	/// with this id: its cost, and before lite-07 its hop chain. The id stays live.
	Update { id: u64, hops: Hops, cost: Cost },
	/// ANNOUNCE_RESTART (lite-07+): the announcement with this id now serves the
	/// instance and epoch given. Another instance is another stream, so whatever the
	/// receiver resolved under its prefix is stale; a higher epoch of the same one
	/// continues it. The id stays live.
	Restart {
		id: u64,
		instance: Option<Instance>,
		epoch: u64,
		cost: Cost,
		hops: Hops,
	},
	/// An unknown lite-06+ announce type. The length-prefixed body was skipped so
	/// the stream stays up; it does not assign an announce id.
	Skipped,
	/// The initial set has been delivered (lite-07+).
	Synced,
	/// ROUTE_START (lite-07+): the sender has a route to the origin node `node`, and
	/// assigns it the next Route ID.
	RouteStart { node: route::Id, advert: Advert },
	/// ROUTE_UPDATE (lite-07+): the route with this id changed, and is usable.
	RouteUpdate { id: u64, advert: Advert },
	/// ROUTE_HOLD (lite-07+): the sender has no usable route with this id, but still
	/// holds its advertisements while it recovers.
	RouteHold { id: u64 },
	/// ROUTE_END (lite-07+): the sender let go of the route with this id, and of every
	/// advertisement under it. The receiver echoes it back.
	RouteEnd { id: u64 },
	/// ANNOUNCE_FLOOR (lite-07+): closes the batch of advertisements under the route
	/// with this id, certifying them across the whole requested prefix at the origin's
	/// revision, or certifying nothing (`None`).
	Floor { id: u64, revision: Option<route::Rev> },
	/// WARM_START (lite-07+): under the route with Route ID `route`, the sender keeps its
	/// copy of the broadcast at `suffix` whether or not the receiver pulls through it, and
	/// assigns it the next Warm ID. See [`Warm`] for what the copy carries.
	WarmStart {
		route: u64,
		suffix: Path,
		instance: Instance,
		warm: Warm,
	},
	/// WARM_UPDATE (lite-07+): the warm route with this id carries `warm` now.
	WarmUpdate { id: u64, warm: Warm },
	/// WARM_END (lite-07+): the sender withdrew the warm route with this id.
	WarmEnd { id: u64 },
}

/// A copy of a broadcast a warm route offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Warm {
	/// What pulling the copy through the sender costs it: 0 at the copy's carrier, else
	/// its own pull's cost, in the units of the route's metric.
	pub cost: u64,
	/// The position in `hops` of the carrier, the relay with a reader where the copy left
	/// its origin's route.
	pub carrier: u64,
	/// Every node the copy crossed, the origin node first and the sender last.
	pub hops: Arc<[route::Id]>,
}

/// Encode a warm route's cost, carrier, and hops. No relay offers a copy that crossed too
/// many hops, or whose carrier is not among them.
fn encode_warm(w: &mut Encoder<'_>, warm: &Warm) -> Result<(), EncodeError> {
	if warm.hops.len() > route::MAX_HOPS || warm.carrier >= warm.hops.len() as u64 {
		return Err(EncodeError::InvalidState);
	}
	w.varint(warm.cost)?;
	w.varint(warm.carrier)?;
	w.varint(warm.hops.len() as u64)?;
	for hop in warm.hops.iter() {
		w.varint(*hop)?;
	}
	Ok(())
}

fn decode_warm(r: &mut Decoder<'_>) -> Result<Warm, DecodeError> {
	let cost = r.varint()?;
	let carrier = r.varint()?;
	let count = r.varint()?;
	if count > route::MAX_HOPS as u64 {
		return Err(DecodeError::TooMany);
	}
	if carrier >= count {
		return Err(DecodeError::InvalidValue);
	}
	let hops = (0..count).map(|_| r.varint()).collect::<Result<_, _>>()?;
	Ok(Warm { cost, carrier, hops })
}

/// ROUTE_START and ROUTE_UPDATE flag: the route is down-only.
const DOWN_ONLY: u8 = 0x1;
/// ROUTE_START and ROUTE_UPDATE flag: an RTT follows.
const RTT: u8 = 0x2;

/// Encode a seqno, metric, flags, RTT in milliseconds, and the hops the route
/// crossed. No origin advertises the last seqno, which would leave no room for the
/// successor a ROUTE_REQUEST names, and no relay forwards a route that crossed too many.
fn encode_advert(w: &mut Encoder<'_>, advert: &Advert) -> Result<(), EncodeError> {
	if advert.seqno == route::MAX || advert.hops.len() > route::MAX_HOPS {
		return Err(EncodeError::InvalidState);
	}
	w.varint(advert.seqno)?;
	w.varint(advert.metric)?;
	// A sum past the field's range, which only a peer's absurd value reaches, is unknown.
	let rtt = advert.rtt.and_then(|rtt| u64::try_from(rtt.as_millis()).ok());
	let mut flags = 0;
	if advert.down_only {
		flags |= DOWN_ONLY;
	}
	if rtt.is_some() {
		flags |= RTT;
	}
	w.u8(flags);
	if let Some(rtt) = rtt {
		w.varint(rtt)?;
	}
	w.varint(advert.hops.len() as u64)?;
	for hop in advert.hops.iter() {
		w.varint(*hop)?;
	}
	Ok(())
}

fn decode_advert(r: &mut Decoder<'_>) -> Result<Advert, DecodeError> {
	let seqno = r.varint()?;
	if seqno == route::MAX {
		return Err(DecodeError::InvalidValue);
	}
	let metric = r.varint()?;
	let flags = r.u8()?;
	if flags & !(DOWN_ONLY | RTT) != 0 {
		return Err(DecodeError::InvalidValue);
	}
	let rtt = match flags & RTT {
		0 => None,
		_ => Some(std::time::Duration::from_millis(r.varint()?)),
	};
	let count = r.varint()?;
	if count > route::MAX_HOPS as u64 {
		return Err(DecodeError::TooMany);
	}
	let hops = (0..count).map(|_| r.varint()).collect::<Result<_, _>>()?;
	Ok(Advert {
		seqno,
		metric,
		down_only: flags & DOWN_ONLY != 0,
		rtt,
		hops,
	})
}

/// Encode an ANNOUNCE_FLOOR body: the revision plus one, or 0 to certify nothing.
fn encode_floor(w: &mut Encoder<'_>, id: u64, revision: Option<route::Rev>) -> Result<(), EncodeError> {
	w.varint(id)?;
	match revision {
		// No origin advertises the last revision, so it never needs the offset.
		Some(route::MAX) => Err(EncodeError::InvalidState),
		Some(rev) => w.varint(rev + 1),
		None => w.varint(0),
	}
}

fn decode_floor(r: &mut Decoder<'_>) -> Result<AnnounceBroadcast, DecodeError> {
	let id = r.varint()?;
	let revision = r.varint()?.checked_sub(1);
	Ok(AnnounceBroadcast::Floor { id, revision })
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

/// Encode the Route ID a claim travels under, which only lite-07 carries.
fn encode_route_id(w: &mut Encoder<'_>, version: Version, route: u64) -> Result<(), EncodeError> {
	match version.has_routes() {
		true => w.varint(route),
		false if route != 0 => Err(EncodeError::InvalidState),
		false => Ok(()),
	}
}

/// Decode the Route ID a claim travels under, 0 on a version without one.
fn decode_route_id(r: &mut Decoder<'_>, version: Version) -> Result<u64, DecodeError> {
	match version.has_routes() {
		true => r.varint(),
		false => Ok(0),
	}
}

/// Encode a lite-06 announcement's hop chain, which lite-07 replaces with its Route ID.
fn encode_chain(w: &mut Encoder<'_>, version: Version, hops: &Hops) -> Result<(), EncodeError> {
	match version.has_routes() {
		true => Ok(()),
		false => hops.encode(w, version),
	}
}

/// Decode a lite-06 announcement's hop chain, empty on lite-07.
fn decode_chain(r: &mut Decoder<'_>, version: Version) -> Result<Hops, DecodeError> {
	match version.has_routes() {
		true => Ok(Hops::default()),
		false => Hops::decode(r, version),
	}
}

/// Encode lite-07's trailing Lite-06 Chain, omitted when empty. A lite-06 compatibility
/// field: the lite-07 to lite-08 migration deletes it.
fn encode_lite06_chain(w: &mut Encoder<'_>, version: Version, hops: &Hops) -> Result<(), EncodeError> {
	match version.has_routes() && !hops.is_empty() {
		true => hops.encode(w, version),
		false => Ok(()),
	}
}

/// Decode lite-07's trailing Lite-06 Chain: present only when bytes remain, and never
/// empty, so an empty chain has one spelling.
fn decode_lite06_chain(r: &mut Decoder<'_>, version: Version) -> Result<Hops, DecodeError> {
	if !version.has_routes() || r.is_empty() {
		return Ok(Hops::default());
	}
	let hops = Hops::decode(r, version)?;
	match hops.is_empty() {
		true => Err(DecodeError::InvalidValue),
		false => Ok(hops),
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
			let typ = match self {
				Self::Active { .. } => ANNOUNCE_START,
				Self::Synced if version.has_synced() => SYNCED,
				Self::Synced => return Err(EncodeError::Version),
				Self::EndedId { .. } => ANNOUNCE_END,
				Self::Update { .. } => ANNOUNCE_UPDATE,
				Self::Restart { .. } if version.has_announce_restart() => ANNOUNCE_RESTART,
				// Older versions send an end and a start instead.
				Self::Restart { .. } => return Err(EncodeError::Version),
				Self::RouteStart { .. } if version.has_routes() => ROUTE_START,
				Self::RouteUpdate { .. } if version.has_routes() => ROUTE_UPDATE,
				Self::RouteHold { .. } if version.has_routes() => ROUTE_HOLD,
				Self::RouteEnd { .. } if version.has_routes() => ROUTE_END,
				Self::Floor { .. } if version.has_routes() => ANNOUNCE_FLOOR,
				Self::WarmStart { .. } if version.has_warm() => WARM_START,
				Self::WarmUpdate { .. } if version.has_warm() => WARM_UPDATE,
				Self::WarmEnd { .. } if version.has_warm() => WARM_END,
				Self::RouteStart { .. }
				| Self::RouteUpdate { .. }
				| Self::RouteHold { .. }
				| Self::RouteEnd { .. }
				| Self::Floor { .. }
				| Self::WarmStart { .. }
				| Self::WarmUpdate { .. }
				| Self::WarmEnd { .. } => return Err(EncodeError::Version),
				// The pre-lite-06 path-form retraction has no place on lite-06.
				Self::Ended { .. } => return Err(EncodeError::Version),
				// Decode-only: an unknown type is never sent.
				Self::Skipped => return Err(EncodeError::Unsupported),
			};
			w.varint(typ)?;

			let prefix = w.prefix_varint();
			match self {
				Self::Active {
					suffix,
					route,
					instance,
					epoch,
					hops,
					cost,
				} => {
					suffix.encode(w, version)?;
					encode_route_id(w, version, *route)?;
					super::instance::encode_route(w, version, instance.as_ref(), *epoch)?;
					encode_chain(w, version, hops)?;
					cost.encode(w, version)?;
					encode_lite06_chain(w, version, hops)?;
				}
				Self::EndedId { id } => w.varint(*id)?,
				Self::Update { id, hops, cost } => {
					w.varint(*id)?;
					encode_chain(w, version, hops)?;
					cost.encode(w, version)?;
					encode_lite06_chain(w, version, hops)?;
				}
				Self::Restart {
					id,
					instance,
					epoch,
					cost,
					hops,
				} => {
					w.varint(*id)?;
					super::instance::encode_route(w, version, instance.as_ref(), *epoch)?;
					cost.encode(w, version)?;
					encode_lite06_chain(w, version, hops)?;
				}
				Self::Synced => {}
				Self::RouteStart { node, advert } => {
					if *node == 0 {
						return Err(EncodeError::InvalidState);
					}
					w.varint(*node)?;
					encode_advert(w, advert)?;
				}
				Self::RouteUpdate { id, advert } => {
					w.varint(*id)?;
					encode_advert(w, advert)?;
				}
				Self::RouteHold { id } | Self::RouteEnd { id } => w.varint(*id)?,
				Self::Floor { id, revision } => encode_floor(w, *id, *revision)?,
				Self::WarmStart {
					route,
					suffix,
					instance,
					warm,
				} => {
					w.varint(*route)?;
					suffix.encode(w, version)?;
					super::instance::encode_copy(w, instance)?;
					encode_warm(w, warm)?;
				}
				Self::WarmUpdate { id, warm } => {
					w.varint(*id)?;
					encode_warm(w, warm)?;
				}
				Self::WarmEnd { id } => w.varint(*id)?,
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
			Self::Active {
				suffix, route, hops, ..
			} => {
				// Bases are a lite-07 addition; PathRef refuses one here.
				if suffix.base != 0 {
					return Err(EncodeError::Version);
				}
				if *route != 0 {
					return Err(EncodeError::InvalidState);
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
			// The id-referencing forms only exist on lite-06+.
			Self::EndedId { .. }
			| Self::Update { .. }
			| Self::Restart { .. }
			| Self::Skipped
			| Self::Synced
			| Self::RouteStart { .. }
			| Self::RouteUpdate { .. }
			| Self::RouteHold { .. }
			| Self::RouteEnd { .. }
			| Self::Floor { .. }
			| Self::WarmStart { .. }
			| Self::WarmUpdate { .. }
			| Self::WarmEnd { .. } => {
				return Err(EncodeError::Version);
			}
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
					ANNOUNCE_START => {
						let suffix = PathRef::decode(&mut body, version)?;
						let route = decode_route_id(&mut body, version)?;
						let (instance, epoch) = super::instance::decode_route(&mut body, version)?;
						let hops = decode_chain(&mut body, version)?;
						let cost = Cost::decode(&mut body, version)?;
						Self::Active {
							suffix,
							route,
							instance,
							epoch,
							hops: match version.has_routes() {
								true => decode_lite06_chain(&mut body, version)?,
								false => hops,
							},
							cost,
						}
					}
					SYNCED if version.has_synced() => Self::Synced,
					ANNOUNCE_END => Self::EndedId { id: body.varint()? },
					ANNOUNCE_UPDATE => {
						let id = body.varint()?;
						let hops = decode_chain(&mut body, version)?;
						let cost = Cost::decode(&mut body, version)?;
						Self::Update {
							id,
							hops: match version.has_routes() {
								true => decode_lite06_chain(&mut body, version)?,
								false => hops,
							},
							cost,
						}
					}
					ANNOUNCE_RESTART if version.has_announce_restart() => {
						let id = body.varint()?;
						let (instance, epoch) = super::instance::decode_route(&mut body, version)?;
						Self::Restart {
							id,
							instance,
							epoch,
							cost: Cost::decode(&mut body, version)?,
							hops: decode_lite06_chain(&mut body, version)?,
						}
					}
					ROUTE_START if version.has_routes() => {
						let node = body.varint()?;
						let advert = decode_advert(&mut body)?;
						if node == 0 {
							return Err(DecodeError::InvalidValue);
						}
						Self::RouteStart { node, advert }
					}
					ROUTE_UPDATE if version.has_routes() => Self::RouteUpdate {
						id: body.varint()?,
						advert: decode_advert(&mut body)?,
					},
					ROUTE_HOLD if version.has_routes() => Self::RouteHold { id: body.varint()? },
					ROUTE_END if version.has_routes() => Self::RouteEnd { id: body.varint()? },
					ANNOUNCE_FLOOR if version.has_routes() => decode_floor(&mut body)?,
					WARM_START if version.has_warm() => Self::WarmStart {
						route: body.varint()?,
						suffix: Path::decode(&mut body, version)?,
						instance: super::instance::decode_copy(&mut body)?,
						warm: decode_warm(&mut body)?,
					},
					WARM_UPDATE if version.has_warm() => Self::WarmUpdate {
						id: body.varint()?,
						warm: decode_warm(&mut body)?,
					},
					WARM_END if version.has_warm() => Self::WarmEnd { id: body.varint()? },
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
				route: 0,
				instance: None,
				epoch: 0,
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
				route: 0,
				instance: None,
				epoch: 0,
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

/// ANNOUNCE_REQUEST: sent by the subscriber to request ANNOUNCE_BROADCAST messages
/// for a path prefix. Renamed from ANNOUNCE_INTEREST in lite-05.
#[derive(Clone, Debug)]
pub struct AnnounceRequest {
	// Request tracks with this prefix.
	pub prefix: Path,
	// Lite04/05 only: unless unknown, the publisher SHOULD skip announces whose hop IDs
	// contain this value. Not on the wire elsewhere, so the value set here is ignored
	// when encoding for another version and decodes as unknown; lite-06 carries the
	// identity session-wide in the SETUP Hop parameter instead.
	pub exclude_hop: Hop,
	// Lite07+: also announce routes with a `.`-prefixed segment below the prefix.
	// Not on the wire earlier, so the value set here is ignored when encoding for an
	// older version and decodes as false.
	pub hidden: bool,
}

impl<'a> Message<'a> for AnnounceRequest {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Announce);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		let prefix = Path::decode(r, version)?;
		let exclude_hop = match version.has_exclude_hop() {
			true => Hop::decode(r, version)?,
			false => Hop::UNKNOWN,
		};
		let hidden = match version.has_hidden() {
			true => r.bool()?,
			false => false,
		};
		Ok(Self {
			prefix,
			exclude_hop,
			hidden,
		})
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		self.prefix.encode(w, version)?;
		if version.has_exclude_hop() {
			self.exclude_hop.encode(w, version)?;
		}
		if version.has_hidden() {
			w.bool(self.hidden);
		}

		Ok(())
	}
}

/// A message the subscriber sends on the Announce Stream after its ANNOUNCE_REQUEST
/// (lite-07+), toward the publisher that sent the routes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnounceReply {
	/// ROUTE_REQUEST: asks for a route to `node`, as the publisher named it, at least
	/// as new as `seqno`.
	RouteRequest { node: route::Id, seqno: route::Seqno },
	/// The echo of the ROUTE_END that retired this Route ID, sent once the subscriber
	/// dropped the route.
	RouteEnd { id: u64 },
}

impl Encode<Version> for AnnounceReply {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		w.control = Some(crate::stats::Message::Announce);
		if !version.has_routes() {
			return Err(EncodeError::Version);
		}
		w.varint(match self {
			Self::RouteRequest { .. } => ROUTE_REQUEST,
			Self::RouteEnd { .. } => ROUTE_END,
		})?;
		let prefix = w.prefix_varint();
		match self {
			Self::RouteRequest { node: 0, .. } => return Err(EncodeError::InvalidState),
			Self::RouteRequest { node, seqno } => {
				w.varint(*node)?;
				w.varint(*seqno)?;
			}
			Self::RouteEnd { id } => w.varint(*id)?,
		}
		w.fill(prefix)
	}
}

impl<'a> Decode<'a, Version> for AnnounceReply {
	fn decode(buf: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		buf.control = Some(crate::stats::Message::Announce);
		if !version.has_routes() {
			return Err(DecodeError::Version);
		}
		let typ = buf.varint()?;
		let size = decode_size(buf, MAX_MESSAGE_SIZE)?;
		let mut body = buf.sub(size)?;
		let msg = (|| {
			Ok(match typ {
				ROUTE_REQUEST => match body.varint()? {
					0 => return Err(DecodeError::InvalidValue),
					node => Self::RouteRequest {
						node,
						seqno: body.varint()?,
					},
				},
				ROUTE_END => Self::RouteEnd { id: body.varint()? },
				typ => return Err(DecodeError::InvalidMessage(typ)),
			})
		})()
		.map_err(DecodeError::complete)?;
		if !body.is_empty() {
			return Err(DecodeError::Long);
		}
		Ok(msg)
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
/// following. Lite07 has an empty body: its claims name their route instead of a hop
/// chain, and SYNCED marks the initial set, so neither field is sent and both decode
/// as unknown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnounceOk {
	pub origin: Hop,
	pub active: u64,
}

impl<'a> Message<'a> for AnnounceOk {
	const CONTROL: Option<crate::stats::Message> = Some(crate::stats::Message::Announce);
	fn decode_msg(r: &mut Decoder<'a>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_announce_ok() {
			return Err(DecodeError::Version);
		}
		if version.has_routes() {
			return Ok(Self {
				origin: Hop::UNKNOWN,
				active: 0,
			});
		}

		let origin = Hop::decode(r, version)?;
		let active = r.varint()?;
		Ok(Self { origin, active })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_announce_ok() {
			return Err(EncodeError::Version);
		}
		if version.has_routes() {
			return Ok(());
		}

		self.origin.encode(w, version)?;
		w.varint(self.active)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::Buf;

	/// A legacy initial set is held to the announce budget before any suffix decodes, so
	/// a body of one-byte suffixes cannot become a list many times its size.
	#[test]
	fn announce_init_is_bounded_by_the_budget() {
		let version = Version::Lite02;
		let mut wire = Vec::new();
		AnnounceInit {
			suffixes: vec![Path::from_static("a"), Path::from_static(""), Path::from_static("b")],
		}
		.encode(&mut Encoder::new(&mut wire, version.into()), version)
		.unwrap();
		let decode =
			|wire: &[u8], budget| AnnounceInit::decode_within(&mut Decoder::new(wire, version.into()), version, budget);

		assert_eq!(decode(&wire, 3).unwrap().suffixes.len(), 3);
		assert!(matches!(decode(&wire, 2), Err(DecodeError::TooMany)));

		// A count the body cannot hold is malformed whatever the budget, before any allocation.
		let mut wire = Vec::new();
		let mut w = Encoder::new(&mut wire, version.into());
		w.varint(8).unwrap();
		w.varint(u32::MAX.into()).unwrap();
		assert!(matches!(decode(&wire, usize::MAX), Err(DecodeError::InvalidValue)));
	}

	/// Lite-07's ANNOUNCE_OK is empty: SYNCED replaces the count, and routes replace the
	/// Hop ID. Lite-06 keeps both.
	#[test]
	fn announce_ok_is_empty_on_lite07() {
		for version in [Version::Lite06, Version::Lite07] {
			let mut bytes = Vec::new();
			AnnounceOk {
				origin: Hop::new(1).unwrap(),
				active: 7,
			}
			.encode(&mut Encoder::new(&mut bytes, version.into()), version)
			.unwrap();
			assert_eq!(
				bytes,
				if version == Version::Lite06 {
					vec![2, 1, 7]
				} else {
					vec![0]
				}
			);
		}
		assert_eq!(
			crate::coding::decode_buf(&mut &[0u8][..], Version::Lite07, |r, v| AnnounceOk::decode(r, v)).unwrap(),
			AnnounceOk {
				origin: Hop::UNKNOWN,
				active: 0,
			}
		);
		// A body where the draft has none is malformed.
		assert!(
			crate::coding::decode_buf(&mut &[1u8, 1][..], Version::Lite07, |r, v| AnnounceOk::decode(r, v)).is_err()
		);
	}

	#[test]
	fn synced_marks_lite07s_initial_set() {
		let mut bytes = Vec::new();
		AnnounceBroadcast::Synced
			.encode(&mut Encoder::new(&mut bytes, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		assert_eq!(bytes, vec![4, 0]);
		assert!(matches!(
			AnnounceBroadcast::Synced.encode(
				&mut Encoder::new(&mut Vec::new(), Version::Lite06.into()),
				Version::Lite06
			),
			Err(EncodeError::Version)
		));
	}

	// Forge an ANNOUNCE_BROADCAST with the draft's explicit `restart` status (2) for the given version.
	fn encode_forged_restart(version: Version) -> bytes::Bytes {
		// Encode a normal Active, then flip its status byte (1 -> 2).
		let mut buf = Vec::new();
		AnnounceBroadcast::Active {
			route: 0,
			instance: None,
			epoch: 0,
			suffix: PathRef::literal(Path::from_static("foo/bar")),
			hops: Hops::default(),
			cost: Cost::default(),
		}
		.encode(&mut Encoder::new(&mut buf, version.into()), version)
		.expect("encode");

		// Layout: <size varint><status u8><...>. The message is small, so the size is one byte and
		// the status byte sits at index 1.
		assert_eq!(
			buf[1],
			u8::from(AnnounceStatus::Active),
			"expected an Active status byte"
		);
		buf[1] = u8::from(AnnounceStatus::Update);
		bytes::Bytes::from(buf)
	}

	// On lite-05+ the explicit `restart` status is accepted and surfaced as an `Active` (the
	// subscriber retires an already-announced path before republishing it).
	#[test]
	fn decodes_explicit_restart_status_as_active_on_lite05() {
		let version = Version::Lite05;
		let mut slice = encode_forged_restart(version);
		let decoded = crate::coding::decode_buf(&mut slice, version, |r, v| AnnounceBroadcast::decode(r, v))
			.expect("explicit restart must decode");
		assert!(!slice.has_remaining(), "trailing bytes after decode");
		assert!(
			matches!(decoded, AnnounceBroadcast::Active { .. }),
			"restart should decode as Active"
		);
	}

	// Older versions never defined the restart status, so it's an invalid value there.
	#[test]
	fn rejects_explicit_restart_status_before_lite05() {
		let version = Version::Lite04;
		let mut slice = encode_forged_restart(version);
		assert!(
			matches!(
				crate::coding::decode_buf(&mut slice, version, |r, v| AnnounceBroadcast::decode(r, v)),
				Err(DecodeError::InvalidValue)
			),
			"restart status must be rejected before lite-05"
		);
	}

	fn round_trip(msg: &AnnounceOk) -> AnnounceOk {
		let mut buf = Vec::new();
		msg.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| AnnounceOk::decode(r, v)).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		got
	}

	#[test]
	fn announce_ok_round_trip() {
		let msg = AnnounceOk {
			origin: Hop::new(42).unwrap(),
			active: 3,
		};
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn announce_ok_zero_active() {
		let msg = AnnounceOk {
			origin: Hop::new(7).unwrap(),
			active: 0,
		};
		assert_eq!(round_trip(&msg), msg);
	}

	fn broadcast_round_trip(msg: &AnnounceBroadcast, version: Version) -> AnnounceBroadcast {
		let mut buf = Vec::new();
		msg.encode(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, version, |r, v| AnnounceBroadcast::decode(r, v)).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		got
	}

	#[test]
	fn announce_broadcast_round_trip_on_lite05() {
		let mut hops = Hops::new();
		hops.push(Hop::new(7).unwrap()).unwrap();
		let msg = AnnounceBroadcast::Active {
			route: 0,
			instance: None,
			epoch: 0,
			suffix: PathRef::literal(Path::from_static("room/cam")),
			hops: hops.clone(),
			cost: Cost::UNKNOWN,
		};
		assert_eq!(broadcast_round_trip(&msg, Version::Lite05), msg);

		let ended = AnnounceBroadcast::Ended {
			suffix: Path::from_static("room/cam"),
			hops: Hops::new(),
		};
		assert_eq!(broadcast_round_trip(&ended, Version::Lite05), ended);
	}

	#[test]
	fn announce_broadcast_round_trip_on_lite06() {
		let mut hops = Hops::new();
		hops.push(Hop::new(7).unwrap()).unwrap();

		// A priced start and update preserve the same static value.
		let cost = Cost::new(12);

		let active = AnnounceBroadcast::Active {
			route: 0,
			instance: None,
			epoch: 0,
			suffix: PathRef::literal(Path::from_static("room/cam")),
			hops: hops.clone(),
			cost,
		};
		assert_eq!(broadcast_round_trip(&active, Version::Lite06), active);

		let ended = AnnounceBroadcast::EndedId { id: 3 };
		assert_eq!(broadcast_round_trip(&ended, Version::Lite06), ended);

		let update = AnnounceBroadcast::Update { id: 3, hops, cost };
		assert_eq!(broadcast_round_trip(&update, Version::Lite06), update);
	}

	// Lite07 carries the path base as it travels, and the route in place of a hop chain;
	// the codec resolves nothing.
	#[test]
	fn announce_broadcast_round_trip_on_lite07() {
		let cost = Cost::new(12);

		let active = AnnounceBroadcast::Active {
			route: 4,
			instance: Some("a".parse().unwrap()),
			epoch: 5,
			suffix: PathRef {
				base: 2,
				keep: 3,
				rest: Path::from_static("cam"),
			},
			hops: Hops::default(),
			cost,
		};
		assert_eq!(broadcast_round_trip(&active, Version::Lite07), active);

		let update = AnnounceBroadcast::Update {
			id: 3,
			hops: Hops::default(),
			cost,
		};
		assert_eq!(broadcast_round_trip(&update, Version::Lite07), update);

		for epoch in [0, 9] {
			let restart = AnnounceBroadcast::Restart {
				id: 3,
				instance: Some(Instance::random()),
				epoch,
				cost,
				hops: Hops::default(),
			};
			assert_eq!(broadcast_round_trip(&restart, Version::Lite07), restart);
		}
	}

	/// The draft's field order, byte for byte: ANNOUNCE_START is the path base, keep, and
	/// rest, the Route ID, the instance, its minted flag, the epoch, and the cost;
	/// ANNOUNCE_UPDATE is the Announce ID and the cost.
	#[test]
	fn lite07_announce_layout() {
		let encode = |msg: AnnounceBroadcast| {
			let mut buf = Vec::new();
			msg.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
				.unwrap();
			buf
		};
		let suffix = PathRef {
			base: 1,
			keep: 2,
			rest: Path::from_static("c"),
		};
		assert_eq!(
			encode(AnnounceBroadcast::Active {
				suffix: suffix.clone(),
				route: 3,
				instance: Some("a".parse().unwrap()),
				epoch: 5,
				hops: Hops::default(),
				cost: Cost::new(6),
			}),
			[0, 10, 1, 2, 1, b'c', 3, 1, b'a', 0, 5, 6]
		);
		assert_eq!(
			encode(AnnounceBroadcast::Update {
				id: 7,
				hops: Hops::default(),
				cost: Cost::new(6),
			}),
			[2, 2, 7, 6]
		);
		assert_eq!(
			encode(AnnounceBroadcast::Restart {
				id: 7,
				instance: Some("a".parse().unwrap()),
				epoch: 5,
				cost: Cost::new(6),
				hops: Hops::default(),
			}),
			[3, 6, 7, 1, b'a', 0, 5, 6]
		);
	}

	/// The draft's warm route layouts, byte for byte: WARM_START is the Route ID, the path
	/// suffix, the instance and its minted flag, then the copy's cost, its carrier's
	/// position in its hops, and the hops; WARM_UPDATE the Warm ID and the same three;
	/// WARM_END the Warm ID. Lite-06 skips the types as unknown.
	#[test]
	fn lite07_warm_route_layout() {
		let encode = |msg: &AnnounceBroadcast, version: Version| {
			let mut buf = Vec::new();
			msg.encode(&mut Encoder::new(&mut buf, version.into()), version)
				.map(|()| buf)
		};
		let warm = Warm {
			cost: 4,
			carrier: 1,
			hops: Arc::from([5, 9, 6]),
		};
		let start = AnnounceBroadcast::WarmStart {
			route: 3,
			suffix: Path::from_static("c"),
			instance: "a".parse().unwrap(),
			warm: warm.clone(),
		};
		let update = AnnounceBroadcast::WarmUpdate { id: 7, warm };
		let end = AnnounceBroadcast::WarmEnd { id: 7 };
		let layouts: [(&AnnounceBroadcast, &[u8]); 3] = [
			(&start, &[12, 12, 3, 1, b'c', 1, b'a', 0, 4, 1, 3, 5, 9, 6]),
			(&update, &[13, 7, 7, 4, 1, 3, 5, 9, 6]),
			(&end, &[14, 1, 7]),
		];
		for (msg, wire) in layouts {
			assert_eq!(encode(msg, Version::Lite07).unwrap(), wire);
			assert_eq!(&broadcast_round_trip(msg, Version::Lite07), msg);
			assert!(matches!(encode(msg, Version::Lite06), Err(EncodeError::Version)));
			let got =
				crate::coding::decode_buf(&mut &wire[..], Version::Lite06, |r, v| AnnounceBroadcast::decode(r, v));
			assert_eq!(got.unwrap(), AnnounceBroadcast::Skipped);
		}
	}

	/// A warm route's carrier is one of its hops, of which there are at most 32.
	#[test]
	fn lite07_warm_route_names_its_carrier_among_its_hops() {
		let outside = AnnounceBroadcast::WarmUpdate {
			id: 7,
			warm: Warm {
				cost: 4,
				carrier: 3,
				hops: Arc::from([5, 9, 6]),
			},
		};
		let mut buf = Vec::new();
		assert!(matches!(
			outside.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07),
			Err(EncodeError::InvalidState)
		));
		let decode = |wire: &[u8]| {
			crate::coding::decode_buf(&mut &wire[..], Version::Lite07, |r, v| AnnounceBroadcast::decode(r, v))
		};
		assert!(decode(&[13, 4, 7, 4, 0, 0]).is_err(), "no hops, so no carrier");
		assert!(
			decode(&[13, 7, 7, 4, 3, 3, 5, 9, 6]).is_err(),
			"a carrier past the hops"
		);
		let mut long = vec![13, 37, 7, 4, 0, 33];
		long.extend([1; 33]);
		assert!(decode(&long).is_err(), "33 hops");
		long[1] = 36;
		long[5] = 32;
		long.pop();
		decode(&long).unwrap();
	}

	/// Lite-07 carries a lite-06 chain after the cost, only when it is not empty, so an
	/// explicitly empty one is refused.
	#[test]
	fn lite07_carries_a_trailing_lite06_chain() {
		let mut hops = Hops::new();
		hops.push(Hop::new(7).unwrap()).unwrap();
		hops.push(Hop::new(9).unwrap()).unwrap();
		let encode = |msg: &AnnounceBroadcast| {
			let mut buf = Vec::new();
			msg.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
				.unwrap();
			buf
		};
		let msgs = [
			AnnounceBroadcast::Active {
				route: 3,
				instance: Some("a".parse().unwrap()),
				epoch: 5,
				suffix: PathRef::literal(Path::from_static("c")),
				hops: hops.clone(),
				cost: Cost::new(6),
			},
			AnnounceBroadcast::Update {
				id: 7,
				hops: hops.clone(),
				cost: Cost::new(6),
			},
			AnnounceBroadcast::Restart {
				id: 7,
				instance: Some("a".parse().unwrap()),
				epoch: 5,
				cost: Cost::new(6),
				hops,
			},
		];
		let layouts: [&[u8]; 3] = [
			&[0, 13, 0, 0, 1, b'c', 3, 1, b'a', 0, 5, 6, 2, 7, 9],
			&[2, 5, 7, 6, 2, 7, 9],
			&[3, 9, 7, 1, b'a', 0, 5, 6, 2, 7, 9],
		];
		for (msg, layout) in msgs.iter().zip(layouts) {
			assert_eq!(encode(msg), layout, "{msg:?}");
			assert_eq!(broadcast_round_trip(msg, Version::Lite07), *msg);
		}

		// An ANNOUNCE_UPDATE whose chain is present but empty.
		let mut empty: &[u8] = &[2, 3, 7, 6, 0];
		let res = crate::coding::decode_buf(&mut empty, Version::Lite07, |r, v| AnnounceBroadcast::decode(r, v));
		assert!(res.is_err(), "{res:?}");
	}

	/// Only lite-07 carries a Route ID: a non-zero one has nowhere to go earlier, and an
	/// earlier start decodes as route 0.
	#[test]
	fn a_route_id_needs_lite07() {
		let msg = AnnounceBroadcast::Active {
			route: 1,
			instance: None,
			epoch: 0,
			suffix: PathRef::literal(Path::from_static("cam")),
			hops: Hops::default(),
			cost: Cost::default(),
		};
		for version in [Version::Lite05, Version::Lite06] {
			let mut buf = Vec::new();
			let res = msg.encode(&mut Encoder::new(&mut buf, version.into()), version);
			assert!(matches!(res, Err(EncodeError::InvalidState)), "{version:?}");
		}
	}

	// Only lite-07 carries ANNOUNCE_RESTART: older versions send an end and a start, and an
	// older receiver skips the unknown type.
	#[test]
	fn announce_restart_needs_lite07() {
		let restart = AnnounceBroadcast::Restart {
			id: 1,
			instance: Some("a".parse().unwrap()),
			epoch: 0,
			cost: Cost::default(),
			hops: Hops::default(),
		};
		let mut buf = Vec::new();
		assert!(matches!(
			restart.encode(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06),
			Err(EncodeError::Version)
		));

		let mut buf = Vec::new();
		restart
			.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		// The same bytes, read as lite-06: an unknown type, skipped by length.
		let mut legacy = Vec::new();
		let mut encoder = Encoder::new(&mut legacy, Version::Lite06.into());
		encoder.varint(ANNOUNCE_RESTART).unwrap();
		encoder.varint(1).unwrap();
		encoder.varint(1).unwrap();
		assert_eq!(
			crate::coding::decode_buf(&mut &legacy[..], Version::Lite06, |r, v| AnnounceBroadcast::decode(
				r, v
			))
			.unwrap(),
			AnnounceBroadcast::Skipped
		);
	}

	// A keep copies from a base, so one without a base is malformed.
	#[test]
	fn a_keep_without_a_base_is_rejected() {
		// Path base, path keep, empty rest, route, instance "a", epoch, cost.
		let body = [0, 1, 0, 0, 1, b'a', 0, 0];
		let mut buf = vec![ANNOUNCE_START as u8, body.len() as u8];
		buf.extend_from_slice(&body);
		assert!(matches!(
			crate::coding::decode_buf(&mut &buf[..], Version::Lite07, |r, v| AnnounceBroadcast::decode(r, v)),
			Err(DecodeError::InvalidValue)
		));
	}

	// Only lite-07 has room for a base.
	#[test]
	fn a_base_needs_lite07() {
		let msg = AnnounceBroadcast::Active {
			route: 0,
			instance: None,
			epoch: 0,
			suffix: PathRef {
				base: 1,
				keep: 1,
				rest: Path::from_static("cam"),
			},
			hops: Hops::default(),
			cost: Cost::default(),
		};
		for version in [Version::Lite05, Version::Lite06] {
			let mut buf = Vec::new();
			assert!(matches!(
				msg.encode(&mut Encoder::new(&mut buf, version.into()), version),
				Err(EncodeError::Version)
			));
		}
	}

	// The id-referencing forms don't exist before lite-06, and the path form is gone on lite-06.
	#[test]
	fn announce_broadcast_rejects_cross_version_forms() {
		let mut buf = Vec::new();
		assert!(matches!(
			AnnounceBroadcast::EndedId { id: 1 }
				.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05),
			Err(EncodeError::Version)
		));
		assert!(matches!(
			AnnounceBroadcast::Update {
				id: 1,
				hops: Hops::default(),
				cost: Cost::default()
			}
			.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05),
			Err(EncodeError::Version)
		));
		assert!(matches!(
			AnnounceBroadcast::Ended {
				suffix: Path::from_static("room/cam"),
				hops: Hops::new()
			}
			.encode(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06),
			Err(EncodeError::Version)
		));
	}

	// Pre-lite-06 has no room for a cost on the wire, so one set locally is simply
	// not sent and the peer decodes [`Cost::UNKNOWN`]: free to reach, which keeps a
	// mixed-version mesh ranking those routes on hop count exactly as it did before,
	// with only the arriving link price contributing to route selection.
	#[test]
	fn route_cost_is_dropped_before_lite06() {
		let msg = AnnounceBroadcast::Active {
			route: 0,
			instance: None,
			epoch: 0,
			suffix: PathRef::literal(Path::from_static("room/cam")),
			hops: Hops::default(),
			cost: Cost::new(9),
		};
		let got = broadcast_round_trip(&msg, Version::Lite05);
		assert_eq!(
			got,
			AnnounceBroadcast::Active {
				route: 0,
				instance: None,
				epoch: 0,
				suffix: PathRef::literal(Path::from_static("room/cam")),
				hops: Hops::default(),
				cost: Cost::UNKNOWN,
			}
		);
	}

	// Costs saturate at 2^62-1 on every version, lite-07's 64-bit varints included, so
	// charging a link on top of the ceiling still re-encodes for a peer on any version.
	#[test]
	fn charged_cost_stays_encodable() {
		let cost = Cost::new(u64::MAX).charged(1);
		assert_eq!(cost, Cost::new((1 << 62) - 1));
		assert_eq!(Cost::MAX.charged(1), cost);
		for version in [Version::Lite06, Version::Lite07] {
			let buf = cost.encode_bytes(version).expect("a charged cost must stay encodable");
			assert_eq!(Cost::decode_slice(&buf, version).unwrap().0, cost, "{version}");
		}
	}

	#[test]
	fn unknown_announce_type_is_skipped() {
		let mut body = Vec::new();
		Path::from_static("room/cam")
			.encode(&mut Encoder::new(&mut body, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		Hops::new()
			.encode(&mut Encoder::new(&mut body, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		Cost::default()
			.encode(&mut Encoder::new(&mut body, Version::Lite06.into()), Version::Lite06)
			.unwrap();

		let mut buf = Vec::new();
		Encoder::new(&mut buf, Version::Lite06.into()).varint(4u64).unwrap();
		Encoder::new(&mut buf, Version::Lite06.into())
			.varint(body.len() as u64)
			.unwrap();
		buf.extend_from_slice(&body);

		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, Version::Lite06, |r, v| AnnounceBroadcast::decode(r, v))
			.expect("unknown type must not kill the stream");
		assert!(slice.is_empty());
		assert_eq!(got, AnnounceBroadcast::Skipped);
	}

	// An ANNOUNCE_END message on lite-06 is tiny: type byte, size prefix, id varint.
	#[test]
	fn ended_by_id_is_three_bytes() {
		let mut buf = Vec::new();
		AnnounceBroadcast::EndedId { id: 42 }
			.encode(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		assert_eq!(buf.len(), 3);
	}

	fn request_round_trip(msg: &AnnounceRequest, version: Version) -> AnnounceRequest {
		let mut buf = Vec::new();
		msg.encode(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, version, |r, v| AnnounceRequest::decode(r, v)).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		AnnounceRequest {
			prefix: got.prefix.clone(),
			exclude_hop: got.exclude_hop,
			hidden: got.hidden,
		}
	}

	// Lite07 carries the hidden opt-in; every earlier version decodes as not opted in.
	#[test]
	fn announce_request_carries_hidden_from_lite07() {
		for hidden in [false, true] {
			let msg = AnnounceRequest {
				prefix: Path::from_static("room"),
				exclude_hop: Hop::UNKNOWN,
				hidden,
			};
			assert_eq!(request_round_trip(&msg, Version::Lite07).hidden, hidden);
			assert!(!request_round_trip(&msg, Version::Lite06).hidden);
		}
	}

	// A flag byte other than 0 or 1 is malformed, not a future extension.
	#[test]
	fn announce_request_rejects_a_bad_hidden_flag() {
		let mut buf = Vec::new();
		let mut body = Vec::new();
		Path::from_static("room")
			.encode(&mut Encoder::new(&mut body, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		body.push(2);
		Encoder::new(&mut buf, Version::Lite07.into())
			.varint(body.len() as u64)
			.unwrap();
		buf.extend_from_slice(&body);
		assert!(
			crate::coding::decode_buf(&mut &buf[..], Version::Lite07, |r, v| AnnounceRequest::decode(r, v)).is_err()
		);
	}

	// Lite04/05 carry the subscriber's origin id so the publisher can skip reflected
	// announces before they hit the wire.
	#[test]
	fn announce_request_carries_exclude_hop_on_lite05() {
		let msg = AnnounceRequest {
			prefix: Path::from_static("room"),
			exclude_hop: Hop::new(42).unwrap(),
			hidden: false,
		};
		assert_eq!(request_round_trip(&msg, Version::Lite05).exclude_hop.id(), 42);
	}

	// Lite06 dropped the field: the receiver's reflected-announce check catches the same
	// loops, so a value set locally is simply not sent and decodes as zero.
	#[test]
	fn announce_request_drops_exclude_hop_on_lite06() {
		let msg = AnnounceRequest {
			prefix: Path::from_static("room"),
			exclude_hop: Hop::new(42).unwrap(),
			hidden: false,
		};
		assert_eq!(request_round_trip(&msg, Version::Lite06).exclude_hop, Hop::UNKNOWN);

		// And it costs nothing on the wire: the body is just the prefix.
		let mut with = Vec::new();
		msg.encode(&mut Encoder::new(&mut with, Version::Lite05.into()), Version::Lite05)
			.unwrap();
		let mut without = Vec::new();
		msg.encode(&mut Encoder::new(&mut without, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		assert!(
			without.len() < with.len(),
			"lite06 must not encode the exclude_hop varint"
		);
	}

	#[test]
	fn announce_ok_rejects_old_versions() {
		let msg = AnnounceOk {
			origin: Hop::new(1).unwrap(),
			active: 0,
		};
		let mut buf = Vec::new();
		assert!(matches!(
			msg.encode(&mut Encoder::new(&mut buf, Version::Lite04.into()), Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn announce_ok_accepts_zero_origin() {
		// Encode a well-formed message then patch the origin to 0 on the wire.
		let mut buf = Vec::new();
		AnnounceOk {
			origin: Hop::new(1).unwrap(),
			active: 0,
		}
		.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05)
		.unwrap();
		// origin id 1 sits right after the size prefix; rewrite it to 0.
		let bytes = &buf[..];
		let mut patched = bytes.to_vec();
		// size(1 byte) | origin varint(1 byte = 0x01) | active varint(1 byte)
		patched[1] = 0x00;
		let mut slice = &patched[..];
		let got = crate::coding::decode_buf(&mut slice, Version::Lite05, |r, v| AnnounceOk::decode(r, v)).unwrap();
		assert_eq!(got.origin.id(), 0);
		assert_eq!(got.active, 0);
	}

	#[test]
	fn wip_cost_has_one_field_and_lite06_keeps_two() {
		let mut wire = Vec::new();
		Cost::new(12)
			.encode(&mut Encoder::new(&mut wire, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		assert_eq!(wire, [12]);

		wire.clear();
		Cost::new(12)
			.encode(&mut Encoder::new(&mut wire, Version::Lite06.into()), Version::Lite06)
			.unwrap();
		let mut input = Decoder::new(&wire, Version::Lite06.into());
		assert_eq!(input.varint().unwrap(), 12);
		assert_eq!(input.varint().unwrap(), Cost::MAX.value());
		assert!(input.is_empty());

		for cold in [0, 3, Cost::MAX.value()] {
			wire.clear();
			let mut w = Encoder::new(&mut wire, Version::Lite06.into());
			w.varint(12).unwrap();
			w.varint(cold).unwrap();
			let mut input = Decoder::new(&wire, Version::Lite06.into());
			assert_eq!(Cost::decode(&mut input, Version::Lite06).unwrap(), Cost::new(12));
			assert!(input.is_empty());
		}
	}

	fn advert(seqno: u64, metric: u64, down_only: bool) -> Advert {
		Advert {
			seqno,
			metric,
			down_only,
			rtt: None,
			hops: [].into(),
		}
	}

	/// Encode a typed lite-07 announce message from its raw body, bypassing the encoder's checks.
	fn forge(typ: u64, body: impl FnOnce(&mut Encoder<'_>)) -> Vec<u8> {
		let version = Version::Lite07;
		let mut inner = Vec::new();
		body(&mut Encoder::new(&mut inner, version.into()));
		let mut buf = Vec::new();
		let mut w = Encoder::new(&mut buf, version.into());
		w.varint(typ).unwrap();
		w.varint(inner.len() as u64).unwrap();
		buf.extend_from_slice(&inner);
		buf
	}

	fn decode_broadcast(buf: &[u8], version: Version) -> Result<AnnounceBroadcast, DecodeError> {
		let mut slice = buf;
		crate::coding::decode_buf(&mut slice, version, |r, v| AnnounceBroadcast::decode(r, v))
	}

	fn decode_reply(buf: &[u8], version: Version) -> Result<AnnounceReply, DecodeError> {
		let mut slice = buf;
		crate::coding::decode_buf(&mut slice, version, |r, v| AnnounceReply::decode(r, v))
	}

	#[test]
	fn route_messages_round_trip_on_lite07() {
		for msg in [
			AnnounceBroadcast::RouteStart {
				node: 0x7a3f,
				advert: advert(41, 12, false),
			},
			AnnounceBroadcast::RouteStart {
				node: route::QUALIFIED | 9,
				advert: advert(route::MAX - 1, route::MAX, true),
			},
			AnnounceBroadcast::RouteUpdate {
				id: 0,
				advert: advert(42, 0, true),
			},
			AnnounceBroadcast::RouteStart {
				node: 0x7a40,
				advert: Advert {
					rtt: Some(std::time::Duration::ZERO),
					hops: [5, route::MAX, 9].into(),
					..advert(1, 0, false)
				},
			},
			AnnounceBroadcast::RouteUpdate {
				id: 1,
				advert: Advert {
					rtt: Some(std::time::Duration::from_millis(u64::MAX)),
					hops: (1..=route::MAX_HOPS as u64).collect(),
					..advert(2, 3, true)
				},
			},
			AnnounceBroadcast::RouteHold { id: 3 },
			AnnounceBroadcast::RouteEnd { id: 0 },
			AnnounceBroadcast::Floor {
				id: 0,
				revision: Some(0),
			},
			AnnounceBroadcast::Floor {
				id: 1,
				revision: Some(route::MAX - 1),
			},
			AnnounceBroadcast::Floor { id: 2, revision: None },
		] {
			assert_eq!(broadcast_round_trip(&msg, Version::Lite07), msg);
		}
	}

	/// A peer's largest RTT plus a link goes past the field: it is forwarded as
	/// unknown, never an error that would end the stream.
	#[test]
	fn an_rtt_past_the_field_is_unknown() {
		let msg = |rtt| AnnounceBroadcast::RouteUpdate {
			id: 1,
			advert: Advert {
				rtt,
				..advert(2, 3, false)
			},
		};
		let past = std::time::Duration::from_millis(u64::MAX).checked_add(std::time::Duration::from_millis(1));
		assert!(past.is_some());
		assert_eq!(broadcast_round_trip(&msg(past), Version::Lite07), msg(None));
	}

	/// A floor is a marker: the Route ID and the revision plus one, 0 when it certifies
	/// nothing.
	#[test]
	fn a_floor_is_its_route_and_revision() {
		let encode = |revision| {
			let mut buf = Vec::new();
			AnnounceBroadcast::Floor { id: 0, revision }
				.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
				.unwrap();
			buf
		};
		assert_eq!(encode(Some(0)), vec![9, 2, 0, 1]);
		assert_eq!(encode(Some(7)), vec![9, 2, 0, 8]);
		assert_eq!(encode(None), vec![9, 2, 0, 0]);
	}

	#[test]
	fn route_messages_need_lite07() {
		for msg in [
			AnnounceBroadcast::RouteStart {
				node: 1,
				advert: advert(0, 1, false),
			},
			AnnounceBroadcast::RouteUpdate {
				id: 0,
				advert: advert(0, 1, false),
			},
			AnnounceBroadcast::RouteHold { id: 0 },
			AnnounceBroadcast::RouteEnd { id: 0 },
			AnnounceBroadcast::Floor { id: 0, revision: None },
		] {
			for version in [Version::Lite05, Version::Lite06] {
				let mut buf = Vec::new();
				let res = msg.encode(&mut Encoder::new(&mut buf, version.into()), version);
				assert!(matches!(res, Err(EncodeError::Version)), "{msg:?} on {version:?}");
			}
		}
		// A lite-06 receiver skips the type like any other it does not know.
		let wire = forge(ROUTE_END, |w| w.varint(0u64).unwrap());
		assert_eq!(
			decode_broadcast(&wire, Version::Lite06).unwrap(),
			AnnounceBroadcast::Skipped
		);

		for reply in [
			AnnounceReply::RouteRequest { node: 1, seqno: 0 },
			AnnounceReply::RouteEnd { id: 0 },
		] {
			let mut buf = Vec::new();
			let res = reply.encode(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06);
			assert!(matches!(res, Err(EncodeError::Version)));
		}
		assert!(matches!(
			decode_reply(&wire, Version::Lite06),
			Err(DecodeError::Version)
		));
	}

	/// What no node advertises is refused on both ends: node 0, and the last seqno or
	/// revision.
	#[test]
	fn route_messages_refuse_what_no_node_advertises() {
		let refused = [
			AnnounceBroadcast::RouteStart {
				node: 0,
				advert: advert(0, 1, false),
			},
			AnnounceBroadcast::RouteStart {
				node: 1,
				advert: advert(route::MAX, 1, false),
			},
			AnnounceBroadcast::RouteUpdate {
				id: 0,
				advert: advert(route::MAX, 1, false),
			},
			AnnounceBroadcast::RouteUpdate {
				id: 0,
				advert: Advert {
					hops: (0..=route::MAX_HOPS as u64).collect(),
					..advert(0, 1, false)
				},
			},
			// The last revision has no room for the offset.
			AnnounceBroadcast::Floor {
				id: 0,
				revision: Some(route::MAX),
			},
		];
		for msg in refused {
			let mut buf = Vec::new();
			let res = msg.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07);
			assert!(matches!(res, Err(EncodeError::InvalidState)), "{msg:?}");
		}

		let advert = |node: u64, seqno: u64, metric: u64, down_only: u8| {
			move |w: &mut Encoder<'_>| {
				w.varint(node).unwrap();
				w.varint(seqno).unwrap();
				w.varint(metric).unwrap();
				w.u8(down_only);
				w.varint(0u64).unwrap();
			}
		};
		// A route with `count` hops, naming `named` of them.
		let hops = |count: u64, named: u64| {
			move |w: &mut Encoder<'_>| {
				for field in [1, 0, 1] {
					w.varint(field as u64).unwrap();
				}
				w.u8(0);
				w.varint(count).unwrap();
				for hop in 0..named {
					w.varint(hop + 1).unwrap();
				}
			}
		};
		let over = route::MAX_HOPS as u64 + 1;
		let malformed = [
			// More hops than a route may cross, hops missing, and one too many.
			forge(ROUTE_START, hops(over, over)),
			forge(ROUTE_START, hops(2, 1)),
			forge(ROUTE_START, hops(1, 2)),
			forge(ROUTE_START, advert(0, 0, 1, 0)),
			forge(ROUTE_START, advert(1, route::MAX, 1, 0)),
			// A flag no version defines, and an RTT flag without its field.
			forge(ROUTE_START, advert(1, 0, 1, 4)),
			forge(ROUTE_START, advert(1, 0, 1, 2)),
			// ROUTE_UPDATE's leading field is the Route ID, so 0 is a valid id here.
			forge(ROUTE_UPDATE, advert(0, route::MAX, 1, 0)),
			// A hold carries only its Route ID.
			forge(ROUTE_HOLD, |w| {
				w.varint(0u64).unwrap();
				w.varint(0u64).unwrap();
			}),
			// A floor without its revision, and one with trailing bytes.
			forge(ANNOUNCE_FLOOR, |w| w.varint(0u64).unwrap()),
			forge(ANNOUNCE_FLOOR, |w| {
				w.varint(0u64).unwrap();
				w.varint(1u64).unwrap();
				w.u8(0);
			}),
			// Trailing bytes after a complete body.
			forge(ROUTE_END, |w| {
				w.varint(0u64).unwrap();
				w.u8(0);
			}),
		];
		for wire in malformed {
			assert!(decode_broadcast(&wire, Version::Lite07).is_err(), "{wire:?}");
		}
	}

	#[test]
	fn announce_reply_round_trips_on_lite07() {
		for reply in [
			AnnounceReply::RouteRequest {
				node: 0x7a3f,
				seqno: 42,
			},
			// A request may name the last seqno: only a rotation answers it.
			AnnounceReply::RouteRequest {
				node: route::QUALIFIED | 1,
				seqno: route::MAX,
			},
			AnnounceReply::RouteEnd { id: 0 },
		] {
			let mut buf = Vec::new();
			reply
				.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07)
				.unwrap();
			assert_eq!(decode_reply(&buf, Version::Lite07).unwrap(), reply);
		}
	}

	#[test]
	fn announce_reply_refuses_node_zero_and_unknown_types() {
		let mut buf = Vec::new();
		let res = AnnounceReply::RouteRequest { node: 0, seqno: 1 }
			.encode(&mut Encoder::new(&mut buf, Version::Lite07.into()), Version::Lite07);
		assert!(matches!(res, Err(EncodeError::InvalidState)));

		let zero = forge(ROUTE_REQUEST, |w| {
			w.varint(0u64).unwrap();
			w.varint(1u64).unwrap();
		});
		assert!(matches!(
			decode_reply(&zero, Version::Lite07),
			Err(DecodeError::InvalidValue)
		));
		// Only routes travel back: an announcement type is not a reply.
		let start = forge(ROUTE_START, |w| w.varint(1u64).unwrap());
		assert!(matches!(
			decode_reply(&start, Version::Lite07),
			Err(DecodeError::InvalidMessage(ROUTE_START))
		));
	}
}
