use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{Origin, OriginList, Path, coding::*};

use super::{Message, Version};

/// Whether the negotiated version carries restart (REANNOUNCE) semantics: a duplicate ANNOUNCE
/// (and the draft's explicit `restart` status) for an already-announced path. Older versions never
/// defined this, so we neither send nor interpret it there; a restart is sent as an unannounce
/// followed by a fresh announce instead.
pub fn restart_supported(version: Version) -> bool {
	// Explicitly list older versions so future versions default to supported.
	!matches!(
		version,
		Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04
	)
}

/// The route cost carried on an announcement (lite-06+).
///
/// `base` is set by the original publisher and forwarded unchanged: a standing
/// penalty (or preference) for using this source at all, whatever its distance.
/// `transit` is the accumulated cost of pulling the broadcast along this route:
/// each relay adds its configured cost for the link the announce crossed, and a
/// relay actively carrying the broadcast resets it to zero when forwarding (its
/// upstream path is already paid for, so sharing is free). Routing picks the
/// lowest `base + transit`; with every link at the default cost of 1 and no
/// resets, `transit` equals the hop count, matching pre-lite-06 routing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RouteCost {
	pub base: u64,
	pub transit: u64,
}

impl RouteCost {
	/// The value routing compares: `base + transit`, saturating.
	pub fn total(&self) -> u64 {
		self.base.saturating_add(self.transit)
	}
}

impl Decode<Version> for RouteCost {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let base = u64::decode(r, version)?;
		let transit = u64::decode(r, version)?;
		Ok(Self { base, transit })
	}
}

impl Encode<Version> for RouteCost {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.base.encode(w, version)?;
		self.transit.encode(w, version)
	}
}

/// ANNOUNCE_BROADCAST: sent by the publisher to advertise (or retract) a broadcast,
/// or (lite-06+) to update a live announcement's route cost in place.
///
/// Carries the broadcast path suffix and the hop chain, plus the route cost on
/// lite-06+. Renamed from ANNOUNCE in lite-05.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnounceBroadcast<'a> {
	Active {
		suffix: Path<'a>,
		hops: OriginList,
		/// The route cost (lite-06+). `None` on older versions, where the receiver
		/// derives it from the hop count.
		cost: Option<RouteCost>,
	},
	Ended {
		suffix: Path<'a>,
		hops: OriginList,
	},
	/// ANNOUNCE_UPDATE (lite-06+): mutate a live announcement's route cost without
	/// re-announcing. The hop chain is immutable for an announcement's lifetime;
	/// cost is the only dynamic field, so a hot/cold flip travels as this small
	/// message instead of an Ended+Active pair (which would abort in-flight
	/// subscriptions downstream).
	Update {
		suffix: Path<'a>,
		cost: RouteCost,
	},
}

impl Message for AnnounceBroadcast<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let status = AnnounceStatus::decode(r, version)?;
		let suffix = Path::decode(r, version)?;

		// ANNOUNCE_UPDATE carries no hop chain: the chain is immutable for the
		// announcement's lifetime, only the cost changes.
		if let AnnounceStatus::Update = status {
			if !version.has_route_cost() {
				return Err(DecodeError::InvalidValue);
			}
			let cost = RouteCost::decode(r, version)?;
			return Ok(Self::Update { suffix, cost });
		}

		let hops = match version {
			Version::Lite01 | Version::Lite02 => OriginList::new(),
			Version::Lite03 => {
				// Lite03 sends only a hop count, not individual ids. Fill with UNKNOWN placeholders.
				// push() enforces MAX_HOPS and `?` lifts the overflow to DecodeError::BoundsExceeded.
				let count = u64::decode(r, version)? as usize;
				let mut list = OriginList::new();
				for _ in 0..count {
					list.push(Origin::UNKNOWN)?;
				}
				list
			}
			_ => OriginList::decode(r, version)?,
		};

		// The route cost rides only on Active (an Ended just retracts the path).
		let cost = match status {
			AnnounceStatus::Ended => None,
			_ if version.has_route_cost() => Some(RouteCost::decode(r, version)?),
			_ => None,
		};

		Ok(match status {
			AnnounceStatus::Active => Self::Active { suffix, hops, cost },
			AnnounceStatus::Ended => Self::Ended { suffix, hops },
			// We encode a restart as a duplicate ANNOUNCE (a second `Active`), but on versions that
			// support restart we also accept the draft's explicit `restart` status and treat it the
			// same. For an already-announced path the subscriber turns it into a restart; for an
			// unknown path it's a fresh announce. Older versions never defined this status, so it's
			// an invalid value there.
			AnnounceStatus::Restart if restart_supported(version) => Self::Active { suffix, hops, cost },
			AnnounceStatus::Restart => return Err(DecodeError::InvalidValue),
			AnnounceStatus::Update => unreachable!("handled above"),
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match self {
			Self::Active { suffix, hops, cost } => {
				AnnounceStatus::Active.encode(w, version)?;
				suffix.encode(w, version)?;
				encode_hops(w, version, hops)?;
				if version.has_route_cost() {
					// The sender must supply a cost on versions that carry one.
					cost.as_ref().ok_or(EncodeError::Version)?.encode(w, version)?;
				}
			}
			Self::Ended { suffix, hops } => {
				AnnounceStatus::Ended.encode(w, version)?;
				suffix.encode(w, version)?;
				encode_hops(w, version, hops)?;
			}
			Self::Update { suffix, cost } => {
				if !version.has_route_cost() {
					return Err(EncodeError::Version);
				}
				AnnounceStatus::Update.encode(w, version)?;
				suffix.encode(w, version)?;
				cost.encode(w, version)?;
			}
		}

		Ok(())
	}
}

fn encode_hops<W: bytes::BufMut>(w: &mut W, version: Version, hops: &OriginList) -> Result<(), EncodeError> {
	match version {
		Version::Lite01 | Version::Lite02 => Ok(()),
		Version::Lite03 => (hops.len() as u64).encode(w, version),
		_ => hops.encode(w, version),
	}
}

/// ANNOUNCE_REQUEST: sent by the subscriber to request ANNOUNCE_BROADCAST messages
/// for a path prefix. Renamed from ANNOUNCE_INTEREST in lite-05.
#[derive(Clone, Debug)]
pub struct AnnounceRequest<'a> {
	// Request tracks with this prefix.
	pub prefix: Path<'a>,
	// If non-zero, the publisher SHOULD skip announces whose hop IDs contain this value.
	pub exclude_hop: u64,
}

impl Message for AnnounceRequest<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let prefix = Path::decode(r, version)?;
		let exclude_hop = match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => 0,
			_ => u64::decode(r, version)?,
		};
		Ok(Self { prefix, exclude_hop })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.prefix.encode(w, version)?;
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {}
			_ => {
				self.exclude_hop.encode(w, version)?;
			}
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
	/// The draft's explicit restart status. We never encode it (a restart goes out as a duplicate
	/// `Active`), but we accept it on decode for forward/cross-compatibility.
	Restart = 2,
	/// ANNOUNCE_UPDATE (lite-06+): a route-cost update for a live announcement.
	Update = 3,
}

impl Decode<Version> for AnnounceStatus {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let status = u8::decode(r, version)?;
		status.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for AnnounceStatus {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		(*self as u8).encode(w, version)
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
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				return Err(DecodeError::Version);
			}
		}

		let count = u64::decode(r, version)?;

		// Don't allocate more than 1024 elements upfront
		let mut paths = Vec::with_capacity(count.min(1024) as usize);

		for _ in 0..count {
			paths.push(Path::decode(r, version)?);
		}

		Ok(Self { suffixes: paths })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				return Err(EncodeError::Version);
			}
		}

		(self.suffixes.len() as u64).encode(w, version)?;
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
	pub origin: Origin,
	pub active: u64,
}

impl Message for AnnounceOk {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_announce_ok() {
			return Err(DecodeError::Version);
		}

		let origin = Origin::decode(r, version)?;
		let active = u64::decode(r, version)?;
		Ok(Self { origin, active })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_announce_ok() {
			return Err(EncodeError::Version);
		}

		self.origin.encode(w, version)?;
		self.active.encode(w, version)
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::Buf;

	// Forge an ANNOUNCE_BROADCAST with the draft's explicit `restart` status (2) for the given version.
	fn encode_forged_restart(version: Version) -> bytes::Bytes {
		// Encode a normal Active, then flip its status byte (1 -> 2).
		let mut buf = bytes::BytesMut::new();
		AnnounceBroadcast::Active {
			suffix: Path::new("foo/bar"),
			hops: OriginList::new(),
			cost: version.has_route_cost().then(RouteCost::default),
		}
		.encode(&mut buf, version)
		.expect("encode");

		// Layout: <size varint><status u8><...>. The message is small, so the size is one byte and
		// the status byte sits at index 1.
		assert_eq!(
			buf[1],
			u8::from(AnnounceStatus::Active),
			"expected an Active status byte"
		);
		buf[1] = u8::from(AnnounceStatus::Restart);
		buf.freeze()
	}

	// On lite-05+ the explicit `restart` status is accepted and surfaced as an `Active` (the
	// subscriber turns it into a restart for an already-announced path).
	#[test]
	fn decodes_explicit_restart_status_as_active_on_lite05() {
		let version = Version::Lite05;
		let mut slice = encode_forged_restart(version);
		let decoded = AnnounceBroadcast::decode(&mut slice, version).expect("explicit restart must decode");
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
				AnnounceBroadcast::decode(&mut slice, version),
				Err(DecodeError::InvalidValue)
			),
			"restart status must be rejected before lite-05"
		);
	}

	fn round_trip(msg: &AnnounceOk) -> AnnounceOk {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, Version::Lite05).unwrap();
		let mut slice = &buf[..];
		let got = AnnounceOk::decode(&mut slice, Version::Lite05).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		got
	}

	#[test]
	fn announce_ok_round_trip() {
		let msg = AnnounceOk {
			origin: Origin::new(42).unwrap(),
			active: 3,
		};
		assert_eq!(round_trip(&msg), msg);
	}

	#[test]
	fn announce_ok_zero_active() {
		let msg = AnnounceOk {
			origin: Origin::new(7).unwrap(),
			active: 0,
		};
		assert_eq!(round_trip(&msg), msg);
	}

	fn broadcast_round_trip(msg: &AnnounceBroadcast, version: Version) -> AnnounceBroadcast<'static> {
		let mut buf = bytes::BytesMut::new();
		msg.encode(&mut buf, version).unwrap();
		let mut slice = &buf[..];
		let got = AnnounceBroadcast::decode(&mut slice, version).unwrap();
		assert!(slice.is_empty(), "trailing bytes after decode");
		// Decode borrows from `buf`; re-own so the value can outlive this frame.
		match got {
			AnnounceBroadcast::Active { suffix, hops, cost } => AnnounceBroadcast::Active {
				suffix: suffix.to_owned(),
				hops,
				cost,
			},
			AnnounceBroadcast::Ended { suffix, hops } => AnnounceBroadcast::Ended {
				suffix: suffix.to_owned(),
				hops,
			},
			AnnounceBroadcast::Update { suffix, cost } => AnnounceBroadcast::Update {
				suffix: suffix.to_owned(),
				cost,
			},
		}
	}

	#[test]
	fn announce_broadcast_round_trip_on_lite05() {
		let mut hops = OriginList::new();
		hops.push(Origin::new(7).unwrap()).unwrap();
		let msg = AnnounceBroadcast::Active {
			suffix: Path::new("room/cam"),
			hops: hops.clone(),
			cost: None,
		};
		assert_eq!(broadcast_round_trip(&msg, Version::Lite05), msg);

		let ended = AnnounceBroadcast::Ended {
			suffix: Path::new("room/cam"),
			hops: OriginList::new(),
		};
		assert_eq!(broadcast_round_trip(&ended, Version::Lite05), ended);
	}

	#[test]
	fn announce_ok_rejects_old_versions() {
		let msg = AnnounceOk {
			origin: Origin::new(1).unwrap(),
			active: 0,
		};
		let mut buf = bytes::BytesMut::new();
		assert!(matches!(
			msg.encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn announce_ok_accepts_zero_origin() {
		// Encode a well-formed message then patch the origin to 0 on the wire.
		let mut buf = bytes::BytesMut::new();
		AnnounceOk {
			origin: Origin::new(1).unwrap(),
			active: 0,
		}
		.encode(&mut buf, Version::Lite05)
		.unwrap();
		// origin id 1 sits right after the size prefix; rewrite it to 0.
		let bytes = &buf[..];
		let mut patched = bytes.to_vec();
		// size(1 byte) | origin varint(1 byte = 0x01) | active varint(1 byte)
		patched[1] = 0x00;
		let mut slice = &patched[..];
		let got = AnnounceOk::decode(&mut slice, Version::Lite05).unwrap();
		assert_eq!(got.origin.id(), 0);
		assert_eq!(got.active, 0);
	}

	// Lite06 carries the route cost on Active and round-trips ANNOUNCE_UPDATE.
	#[test]
	fn announce_cost_round_trip_on_lite06() {
		let mut hops = OriginList::new();
		hops.push(Origin::new(7).unwrap()).unwrap();
		let msg = AnnounceBroadcast::Active {
			suffix: Path::new("room/cam"),
			hops,
			cost: Some(RouteCost { base: 10, transit: 3 }),
		};
		assert_eq!(broadcast_round_trip(&msg, Version::Lite06Wip), msg);

		let update = AnnounceBroadcast::Update {
			suffix: Path::new("room/cam"),
			cost: RouteCost { base: 0, transit: 2 },
		};
		assert_eq!(broadcast_round_trip(&update, Version::Lite06Wip), update);
	}

	// Encoding an Active without a cost on lite-06 is a caller bug, not a silent default.
	#[test]
	fn announce_active_requires_cost_on_lite06() {
		let msg = AnnounceBroadcast::Active {
			suffix: Path::new("room/cam"),
			hops: OriginList::new(),
			cost: None,
		};
		let mut buf = bytes::BytesMut::new();
		assert!(matches!(
			msg.encode(&mut buf, Version::Lite06Wip),
			Err(EncodeError::Version)
		));
	}

	// ANNOUNCE_UPDATE doesn't exist before lite-06: encode refuses, decode rejects the status.
	#[test]
	fn announce_update_rejected_before_lite06() {
		let update = AnnounceBroadcast::Update {
			suffix: Path::new("room/cam"),
			cost: RouteCost::default(),
		};
		let mut buf = bytes::BytesMut::new();
		assert!(matches!(
			update.encode(&mut buf, Version::Lite05),
			Err(EncodeError::Version)
		));

		// Forge an Update status byte on a lite-05 message.
		let mut buf = bytes::BytesMut::new();
		AnnounceBroadcast::Active {
			suffix: Path::new("foo"),
			hops: OriginList::new(),
			cost: None,
		}
		.encode(&mut buf, Version::Lite05)
		.unwrap();
		buf[1] = u8::from(AnnounceStatus::Update);
		let mut slice = buf.freeze();
		assert!(matches!(
			AnnounceBroadcast::decode(&mut slice, Version::Lite05),
			Err(DecodeError::InvalidValue)
		));
	}
}
