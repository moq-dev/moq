use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{Epoch, Hop, Hops, Path, coding::*, origin::Cost};

use super::{
	Message, Version,
	message::{MAX_MESSAGE_SIZE, decode_size},
};

// lite-06 announce message types: an outer discriminator carried before the length
// prefix, so each announcement is an independently-typed, length-delimited message
// (mirroring SUBSCRIBE_START/END/DROP on the subscribe stream).
const ANNOUNCE_START: u64 = 0;
const ANNOUNCE_END: u64 = 1;
const ANNOUNCE_RESTART: u64 = 2;

/// Whether the negotiated version carries restart (REANNOUNCE) semantics. On lite-05 a restart
/// travels as a duplicate ANNOUNCE (a second `active` for an already-announced path); on lite-06+
/// it is the explicit `restart` status referencing an announce id. Older versions never defined
/// this, so we neither send nor interpret it there; their peers keep the hop chain from the
/// original announce.
pub fn restart_supported(version: Version) -> bool {
	// Explicitly list older versions so future versions default to supported.
	!matches!(
		version,
		Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04
	)
}

/// An announcement on the Announce Stream, advertising or retracting a broadcast.
///
/// On lite-06+ these are independently-typed messages (`ANNOUNCE_START`,
/// `ANNOUNCE_END`, `ANNOUNCE_RESTART`), each framed as `Type | Length | Body`
/// like the subscribe stream's responses. Each `Active` (ANNOUNCE_START)
/// implicitly assigns the next announce id (a per-stream ordinal starting at
/// 0); `EndedId` (ANNOUNCE_END) and `Restart` (ANNOUNCE_RESTART) reference
/// that id instead of repeating the path. Older versions send a single
/// `ANNOUNCE_BROADCAST` message that retracts by path (`Ended`).
///
/// The path and hop chain are as they appear on the wire: on lite-07 they may name a
/// base announcement, resolved against the stream's history by
/// [`AnnounceDecoder`](super::AnnounceDecoder).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnounceBroadcast<'a> {
	/// ANNOUNCE_START (lite-06) / active (older): a broadcast is now available.
	/// Carries the path suffix, the hop chain, and (lite-06+) the warm and cold
	/// route costs, and assigns the next announce id. The epoch (lite-07+) is fixed
	/// for the announcement's lifetime: a new one is an end and a fresh start.
	Active {
		suffix: PathRef<'a>,
		epoch: Option<Epoch>,
		hops: HopsRef,
		cost: Cost,
	},
	/// Pre-lite-06: a broadcast is no longer available, retracted by path.
	Ended { suffix: Path<'a>, hops: Hops },
	/// ANNOUNCE_END (lite-06+): a broadcast is no longer available, retracted by
	/// announce id. The id is retired; referencing it again is a protocol violation.
	EndedId { id: u64 },
	/// ANNOUNCE_RESTART (lite-06+): atomically replace the announcement with this id
	/// (e.g. a new hop chain after a relay failover, or a route whose cost moved).
	/// The id stays live.
	Restart { id: u64, hops: HopsRef, cost: Cost },
	/// An unknown lite-06+ announce type. The length-prefixed body was skipped so
	/// the stream stays up; it does not assign an announce id.
	Skipped,
}

/// A path suffix as it travels: the first `keep` segments of a base announcement's
/// suffix, followed by `rest`.
///
/// `base` is the base's distance back from the stream's next unassigned announce id,
/// so 1 is the latest ANNOUNCE_START, and 0 names no base (`keep` must be 0 too). Only
/// lite-07 carries a base; every other version is `rest` alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathRef<'a> {
	pub base: u64,
	pub keep: u64,
	pub rest: Path<'a>,
}

impl<'a> PathRef<'a> {
	/// The whole suffix, with no base.
	pub fn literal(rest: Path<'a>) -> Self {
		Self { base: 0, keep: 0, rest }
	}
}

impl Encode<Version> for PathRef<'_> {
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

impl Decode<Version> for PathRef<'_> {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
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

/// A hop chain as it travels: `literal` leading hops, followed by the last `keep`
/// entries of a base announcement's chain.
///
/// `base` counts back like [`PathRef::base`], independently of it. Only lite-07
/// carries a base; every other version is `literal` alone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HopsRef {
	pub base: u64,
	pub literal: Hops,
	pub keep: u64,
}

impl HopsRef {
	/// The whole chain, with no base.
	pub fn literal(literal: Hops) -> Self {
		Self {
			base: 0,
			literal,
			keep: 0,
		}
	}
}

impl Encode<Version> for HopsRef {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_announce_compression() {
			if self.base != 0 || self.keep != 0 {
				return Err(EncodeError::Version);
			}
			return self.literal.encode(w, version);
		}
		w.varint(self.base)?;
		self.literal.encode(w, version)?;
		w.varint(self.keep)
	}
}

impl Decode<Version> for HopsRef {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_announce_compression() {
			return Ok(Self::literal(Hops::decode(buf, version)?));
		}
		let base = buf.varint()?;
		let literal = Hops::decode(buf, version)?;
		let keep = buf.varint()?;
		if base == 0 && keep != 0 {
			return Err(DecodeError::InvalidValue);
		}
		Ok(Self { base, literal, keep })
	}
}

impl AnnounceBroadcast<'_> {
	/// Re-own a decoded message so it can outlive the decode buffer.
	#[cfg(test)]
	pub fn into_owned(self) -> AnnounceBroadcast<'static> {
		match self {
			Self::Active {
				suffix,
				epoch,
				hops,
				cost,
			} => AnnounceBroadcast::Active {
				suffix: PathRef {
					base: suffix.base,
					keep: suffix.keep,
					rest: suffix.rest.into_owned(),
				},
				epoch,
				hops,
				cost,
			},
			Self::Ended { suffix, hops } => AnnounceBroadcast::Ended {
				suffix: suffix.into_owned(),
				hops,
			},
			Self::EndedId { id } => AnnounceBroadcast::EndedId { id },
			Self::Restart { id, hops, cost } => AnnounceBroadcast::Restart { id, hops, cost },
			Self::Skipped => AnnounceBroadcast::Skipped,
		}
	}
}

impl Encode<Version> for Cost {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_route_cost() {
			return Ok(());
		}
		w.varint(self.warm)?;
		w.varint(self.cold)
	}
}

impl Decode<Version> for Cost {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_route_cost() {
			return Ok(Cost::UNKNOWN);
		}
		// Costs saturate at 2^62-1 on every version, so a larger one (lite-07's varints
		// reach 2^64-1) reads as the ceiling and still forwards to an older peer.
		let cost = Cost {
			warm: buf.varint()?,
			cold: buf.varint()?,
		};
		Ok(cost.clamped())
	}
}

impl Encode<Version> for AnnounceBroadcast<'_> {
	fn encode(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if version.has_announce_id() {
			// Lite06+: outer type discriminator, then a size-prefixed body (like the
			// subscribe stream). The body varies by type.
			let typ = match self {
				Self::Active { .. } => ANNOUNCE_START,
				Self::EndedId { .. } => ANNOUNCE_END,
				Self::Restart { .. } => ANNOUNCE_RESTART,
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
					epoch,
					hops,
					cost,
				} => {
					suffix.encode(w, version)?;
					super::epoch::encode_epoch(w, version, epoch.as_ref())?;
					hops.encode(w, version)?;
					cost.encode(w, version)?;
				}
				Self::EndedId { id } => w.varint(*id)?,
				Self::Restart { id, hops, cost } => {
					w.varint(*id)?;
					hops.encode(w, version)?;
					cost.encode(w, version)?;
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
				// Bases are a lite-07 addition; PathRef and HopsRef refuse one here.
				if suffix.base != 0 || hops.base != 0 {
					return Err(EncodeError::Version);
				}
				AnnounceStatus::Active.encode(w, version)?;
				suffix.rest.encode(w, version)?;
				encode_hops(w, version, &hops.literal)?;
			}
			Self::Ended { suffix, hops } => {
				AnnounceStatus::Ended.encode(w, version)?;
				suffix.encode(w, version)?;
				encode_hops(w, version, hops)?;
			}
			// The id-referencing forms only exist on lite-06+.
			Self::EndedId { .. } | Self::Restart { .. } | Self::Skipped => {
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

impl Decode<Version> for AnnounceBroadcast<'_> {
	fn decode(buf: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if version.has_announce_id() {
			// Lite06+: outer type, then a size-prefixed body decoded within its bounds.
			let typ = buf.varint()?;
			let size = decode_size(buf, MAX_MESSAGE_SIZE)?;
			let mut body = buf.sub(size)?;
			let msg = match typ {
				ANNOUNCE_START => Self::Active {
					suffix: PathRef::decode(&mut body, version)?,
					epoch: super::epoch::decode_epoch(&mut body, version)?,
					hops: HopsRef::decode(&mut body, version)?,
					cost: Cost::decode(&mut body, version)?,
				},
				ANNOUNCE_END => Self::EndedId { id: body.varint()? },
				ANNOUNCE_RESTART => Self::Restart {
					id: body.varint()?,
					hops: HopsRef::decode(&mut body, version)?,
					cost: Cost::decode(&mut body, version)?,
				},
				// Unknown types are skipped by length so an earlier Lite06 build
				// negotiating the same ALPN does not kill the announce stream.
				_ => {
					body.rest();
					Self::Skipped
				}
			};
			if !body.is_empty() {
				return Err(DecodeError::Long);
			}
			return Ok(msg);
		}

		// Older versions: a single size-prefixed ANNOUNCE_BROADCAST with an inner status.
		let size = decode_size(buf, MAX_MESSAGE_SIZE)?;
		let mut body = buf.sub(size)?;
		let msg = Self::decode_legacy(&mut body, version)?;
		if !body.is_empty() {
			return Err(DecodeError::Long);
		}
		Ok(msg)
	}
}

impl AnnounceBroadcast<'_> {
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
				epoch: None,
				hops: HopsRef::literal(hops),
				cost: Cost::UNKNOWN,
			},
			AnnounceStatus::Ended => Self::Ended { suffix, hops },
			// On lite-05 a restart travels as a duplicate ANNOUNCE (a second `Active`), so accept
			// the draft's explicit `restart` status and treat it the same. Either way the
			// subscriber re-prices an already-announced path in place; for an unknown path it's a
			// fresh announce. Older versions never defined this status, so it's an
			// invalid value there.
			AnnounceStatus::Restart if restart_supported(version) => Self::Active {
				suffix: PathRef::literal(suffix),
				epoch: None,
				hops: HopsRef::literal(hops),
				cost: Cost::UNKNOWN,
			},
			AnnounceStatus::Restart => return Err(DecodeError::InvalidValue),
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
pub struct AnnounceRequest<'a> {
	// Request tracks with this prefix.
	pub prefix: Path<'a>,
	// Lite04/05 only: if non-zero, the publisher SHOULD skip announces whose hop IDs
	// contain this value. Not on the wire elsewhere, so the value set here is ignored
	// when encoding for another version and decodes as zero; lite-06 carries the
	// identity session-wide in the SETUP Hop parameter instead.
	pub exclude_hop: u64,
	// Lite07+: also announce routes with a `.`-prefixed segment below the prefix.
	// Not on the wire earlier, so the value set here is ignored when encoding for an
	// older version and decodes as false.
	pub hidden: bool,
}

impl Message for AnnounceRequest<'_> {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		let prefix = Path::decode(r, version)?;
		let exclude_hop = match version.has_exclude_hop() {
			true => r.varint()?,
			false => 0,
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
			w.varint(self.exclude_hop)?;
		}
		if version.has_hidden() {
			w.bool(self.hidden);
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
	/// The explicit restart status, accepted on decode for forward/cross-compatibility. We never
	/// encode it: a lite-05 restart goes out as a duplicate `Active`.
	Restart = 2,
}

impl Decode<Version> for AnnounceStatus {
	fn decode(r: &mut Decoder<'_>, _: Version) -> Result<Self, DecodeError> {
		let status = r.u8()?;
		status.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for AnnounceStatus {
	fn encode(&self, w: &mut Encoder<'_>, _: Version) -> Result<(), EncodeError> {
		w.u8(*self as u8);
		Ok(())
	}
}

/// Sent after setup to communicate the initially announced paths.
///
/// Used by Draft01/Draft02 only. Draft03 uses individual Announce messages instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnnounceInit<'a> {
	/// List of currently active broadcasts, encoded as suffixes to be combined with the prefix.
	pub suffixes: Vec<Path<'a>>,
}

impl Message for AnnounceInit<'_> {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				return Err(DecodeError::Version);
			}
		}

		let count = r.varint()?;

		// Don't allocate more than 1024 elements upfront
		let mut paths = Vec::with_capacity(count.min(1024) as usize);

		for _ in 0..count {
			paths.push(Path::decode(r, version)?);
		}

		Ok(Self { suffixes: paths })
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
/// receipt instead. `active` is the number of currently-active broadcasts the
/// publisher sends as the initial set immediately after this message, letting the
/// receiver block until the initial set has arrived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnounceOk {
	pub origin: Hop,
	pub active: u64,
}

impl Message for AnnounceOk {
	fn decode_msg(r: &mut Decoder<'_>, version: Version) -> Result<Self, DecodeError> {
		if !version.has_announce_ok() {
			return Err(DecodeError::Version);
		}

		let origin = Hop::decode(r, version)?;
		let active = r.varint()?;
		Ok(Self { origin, active })
	}

	fn encode_msg(&self, w: &mut Encoder<'_>, version: Version) -> Result<(), EncodeError> {
		if !version.has_announce_ok() {
			return Err(EncodeError::Version);
		}

		self.origin.encode(w, version)?;
		w.varint(self.active)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::Buf;

	// Forge an ANNOUNCE_BROADCAST with the draft's explicit `restart` status (2) for the given version.
	fn encode_forged_restart(version: Version) -> bytes::Bytes {
		// Encode a normal Active, then flip its status byte (1 -> 2).
		let mut buf = Vec::new();
		AnnounceBroadcast::Active {
			epoch: None,
			suffix: PathRef::literal(Path::new("foo/bar")),
			hops: HopsRef::default(),
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
		buf[1] = u8::from(AnnounceStatus::Restart);
		bytes::Bytes::from(buf)
	}

	// On lite-05+ the explicit `restart` status is accepted and surfaced as an `Active` (the
	// subscriber retires an already-announced path before republishing it).
	#[test]
	fn decodes_explicit_restart_status_as_active_on_lite05() {
		let version = Version::Lite05;
		let mut slice = encode_forged_restart(version);
		let decoded = crate::coding::decode_buf(&mut slice, version, AnnounceBroadcast::decode)
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
				crate::coding::decode_buf(&mut slice, version, AnnounceBroadcast::decode),
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
		let got = crate::coding::decode_buf(&mut slice, Version::Lite05, AnnounceOk::decode).unwrap();
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

	fn broadcast_round_trip(msg: &AnnounceBroadcast, version: Version) -> AnnounceBroadcast<'static> {
		let mut buf = Vec::new();
		msg.encode(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, version, AnnounceBroadcast::decode).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		got.into_owned()
	}

	#[test]
	fn announce_broadcast_round_trip_on_lite05() {
		let mut hops = Hops::new();
		hops.push(Hop::new(7).unwrap()).unwrap();
		let msg = AnnounceBroadcast::Active {
			epoch: None,
			suffix: PathRef::literal(Path::new("room/cam")),
			hops: HopsRef::literal(hops.clone()),
			cost: Cost::UNKNOWN,
		};
		assert_eq!(broadcast_round_trip(&msg, Version::Lite05), msg);

		let ended = AnnounceBroadcast::Ended {
			suffix: Path::new("room/cam"),
			hops: Hops::new(),
		};
		assert_eq!(broadcast_round_trip(&ended, Version::Lite05), ended);
	}

	#[test]
	fn announce_broadcast_round_trip_on_lite06() {
		let mut hops = Hops::new();
		hops.push(Hop::new(7).unwrap()).unwrap();

		// Asymmetric on purpose: the two magnitudes travel independently, so a
		// swapped or shared encode would round-trip a symmetric pair unnoticed.
		let cost = Cost { warm: 12, cold: 30 };

		let active = AnnounceBroadcast::Active {
			epoch: None,
			suffix: PathRef::literal(Path::new("room/cam")),
			hops: HopsRef::literal(hops.clone()),
			cost,
		};
		assert_eq!(broadcast_round_trip(&active, Version::Lite06), active);

		let ended = AnnounceBroadcast::EndedId { id: 3 };
		assert_eq!(broadcast_round_trip(&ended, Version::Lite06), ended);

		let restart = AnnounceBroadcast::Restart {
			id: 3,
			hops: HopsRef::literal(hops),
			cost,
		};
		assert_eq!(broadcast_round_trip(&restart, Version::Lite06), restart);
	}

	// Lite07 carries both bases as they travel; the codec resolves nothing.
	#[test]
	fn announce_broadcast_round_trip_on_lite07() {
		let mut hops = Hops::new();
		hops.push(Hop::new(7).unwrap()).unwrap();
		let cost = Cost { warm: 12, cold: 30 };

		let active = AnnounceBroadcast::Active {
			epoch: None,
			suffix: PathRef {
				base: 2,
				keep: 3,
				rest: Path::new("cam"),
			},
			hops: HopsRef {
				base: 1,
				literal: hops.clone(),
				keep: 2,
			},
			cost,
		};
		assert_eq!(broadcast_round_trip(&active, Version::Lite07), active);

		let restart = AnnounceBroadcast::Restart {
			id: 3,
			hops: HopsRef {
				base: 4,
				literal: hops,
				keep: 1,
			},
			cost,
		};
		assert_eq!(broadcast_round_trip(&restart, Version::Lite07), restart);
	}

	// A keep copies from a base, so one without a base is malformed.
	#[test]
	fn a_keep_without_a_base_is_rejected() {
		for (path_keep, hop_keep) in [(1u8, 0u8), (0, 1)] {
			// Path base, path keep, empty rest, no epoch, hop base, no hops, hop keep, cost.
			let body = [0, path_keep, 0, 0, 0, 0, hop_keep, 0, 0];
			let mut buf = vec![ANNOUNCE_START as u8, body.len() as u8];
			buf.extend_from_slice(&body);
			assert!(matches!(
				crate::coding::decode_buf(&mut &buf[..], Version::Lite07, AnnounceBroadcast::decode),
				Err(DecodeError::InvalidValue)
			));
		}
	}

	// Only lite-07 has room for a base.
	#[test]
	fn a_base_needs_lite07() {
		let msg = AnnounceBroadcast::Active {
			epoch: None,
			suffix: PathRef {
				base: 1,
				keep: 1,
				rest: Path::new("cam"),
			},
			hops: HopsRef::default(),
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
			AnnounceBroadcast::Restart {
				id: 1,
				hops: HopsRef::default(),
				cost: Cost::default()
			}
			.encode(&mut Encoder::new(&mut buf, Version::Lite05.into()), Version::Lite05),
			Err(EncodeError::Version)
		));
		assert!(matches!(
			AnnounceBroadcast::Ended {
				suffix: Path::new("room/cam"),
				hops: Hops::new()
			}
			.encode(&mut Encoder::new(&mut buf, Version::Lite06.into()), Version::Lite06),
			Err(EncodeError::Version)
		));
	}

	// Pre-lite-06 has no room for a cost on the wire, so one set locally is simply
	// not sent and the peer decodes [`Cost::UNKNOWN`]: free to reach, which keeps a
	// mixed-version mesh ranking those routes on hop count exactly as it did before,
	// with a cold path that ranks last rather than pretending to be the publisher's.
	#[test]
	fn route_cost_is_dropped_before_lite06() {
		let msg = AnnounceBroadcast::Active {
			epoch: None,
			suffix: PathRef::literal(Path::new("room/cam")),
			hops: HopsRef::default(),
			cost: Cost { warm: 9, cold: 9 },
		};
		let got = broadcast_round_trip(&msg, Version::Lite05);
		assert_eq!(
			got,
			AnnounceBroadcast::Active {
				epoch: None,
				suffix: PathRef::literal(Path::new("room/cam")),
				hops: HopsRef::default(),
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
		Path::new("room/cam")
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
		let got = crate::coding::decode_buf(&mut slice, Version::Lite06, AnnounceBroadcast::decode)
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

	fn request_round_trip(msg: &AnnounceRequest, version: Version) -> AnnounceRequest<'static> {
		let mut buf = Vec::new();
		msg.encode(&mut Encoder::new(&mut buf, version.into()), version)
			.unwrap();
		let mut slice = &buf[..];
		let got = crate::coding::decode_buf(&mut slice, version, AnnounceRequest::decode).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		AnnounceRequest {
			prefix: got.prefix.to_owned(),
			exclude_hop: got.exclude_hop,
			hidden: got.hidden,
		}
	}

	// Lite07 carries the hidden opt-in; every earlier version decodes as not opted in.
	#[test]
	fn announce_request_carries_hidden_from_lite07() {
		for hidden in [false, true] {
			let msg = AnnounceRequest {
				prefix: Path::new("room/"),
				exclude_hop: 0,
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
		Path::new("room")
			.encode(&mut Encoder::new(&mut body, Version::Lite07.into()), Version::Lite07)
			.unwrap();
		body.push(2);
		Encoder::new(&mut buf, Version::Lite07.into())
			.varint(body.len() as u64)
			.unwrap();
		buf.extend_from_slice(&body);
		assert!(crate::coding::decode_buf(&mut &buf[..], Version::Lite07, AnnounceRequest::decode).is_err());
	}

	// Lite04/05 carry the subscriber's origin id so the publisher can skip reflected
	// announces before they hit the wire.
	#[test]
	fn announce_request_carries_exclude_hop_on_lite05() {
		let msg = AnnounceRequest {
			prefix: Path::new("room/"),
			exclude_hop: 42,
			hidden: false,
		};
		assert_eq!(request_round_trip(&msg, Version::Lite05).exclude_hop, 42);
	}

	// Lite06 dropped the field: the receiver's reflected-announce check catches the same
	// loops, so a value set locally is simply not sent and decodes as zero.
	#[test]
	fn announce_request_drops_exclude_hop_on_lite06() {
		let msg = AnnounceRequest {
			prefix: Path::new("room/"),
			exclude_hop: 42,
			hidden: false,
		};
		assert_eq!(request_round_trip(&msg, Version::Lite06).exclude_hop, 0);

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
		let got = crate::coding::decode_buf(&mut slice, Version::Lite05, AnnounceOk::decode).unwrap();
		assert_eq!(got.origin.id(), 0);
		assert_eq!(got.active, 0);
	}
}
