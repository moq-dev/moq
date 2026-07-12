use bytes::{Buf, BufMut};
use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{Origin, OriginList, Path, coding::*};

use super::{Message, Version};

// lite-06 announce message types: an outer discriminator carried before the length
// prefix, so each announcement is an independently-typed, length-delimited message
// (mirroring SUBSCRIBE_START/END/DROP on the subscribe stream).
const ANNOUNCE_START: u64 = 0;
const ANNOUNCE_END: u64 = 1;
const ANNOUNCE_RESTART: u64 = 2;

/// Whether the negotiated version carries restart (REANNOUNCE) semantics. On lite-05 a restart
/// travels as a duplicate ANNOUNCE (a second `active` for an already-announced path); on lite-06+
/// it is the explicit `restart` status referencing an announce id. Older versions never defined
/// this, so we neither send nor interpret it there; a restart is sent as an unannounce followed
/// by a fresh announce instead.
pub fn restart_supported(version: Version) -> bool {
	// Explicitly list older versions so future versions default to supported.
	!matches!(
		version,
		Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04
	)
}

/// An announcement on the Announce Stream, advertising or retracting a broadcast.
///
/// On lite-06+ these are three independently-typed messages (`ANNOUNCE_START`,
/// `ANNOUNCE_END`, `ANNOUNCE_RESTART`), each framed as `Type | Length | Body` like
/// the subscribe stream's responses. Each `Active` (ANNOUNCE_START) implicitly assigns
/// the next announce id (a per-stream ordinal starting at 0); `EndedId` (ANNOUNCE_END)
/// and `Restart` (ANNOUNCE_RESTART) reference that id instead of repeating the path.
/// Older versions send a single `ANNOUNCE_BROADCAST` message that retracts by path
/// (`Ended`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnounceBroadcast<'a> {
	/// ANNOUNCE_START (lite-06) / active (older): a broadcast is now available.
	/// Carries the path suffix and the hop chain, and assigns the next announce id.
	Active { suffix: Path<'a>, hops: OriginList },
	/// Pre-lite-06: a broadcast is no longer available, retracted by path.
	Ended { suffix: Path<'a>, hops: OriginList },
	/// ANNOUNCE_END (lite-06+): a broadcast is no longer available, retracted by
	/// announce id. The id is retired; referencing it again is a protocol violation.
	EndedId { id: u64 },
	/// ANNOUNCE_RESTART (lite-06+): atomically replace the announcement with this id
	/// (e.g. a new hop chain after a relay failover). The id stays live.
	Restart { id: u64, hops: OriginList },
}

impl Encode<Version> for AnnounceBroadcast<'_> {
	fn encode<W: BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if version.has_announce_id() {
			// Lite06+: outer type discriminator, then a size-prefixed body (like the
			// subscribe stream). The body varies by type. Announce messages are small and
			// infrequent, so the scratch buffer is cheap.
			let mut body = Vec::new();
			let typ = match self {
				Self::Active { suffix, hops } => {
					suffix.encode(&mut body, version)?;
					hops.encode(&mut body, version)?;
					ANNOUNCE_START
				}
				Self::EndedId { id } => {
					id.encode(&mut body, version)?;
					ANNOUNCE_END
				}
				Self::Restart { id, hops } => {
					id.encode(&mut body, version)?;
					hops.encode(&mut body, version)?;
					ANNOUNCE_RESTART
				}
				// The pre-lite-06 path-form retraction has no place on lite-06.
				Self::Ended { .. } => return Err(EncodeError::Version),
			};
			typ.encode(w, version)?;
			(body.len() as u64).encode(w, version)?;
			w.put_slice(&body);
			return Ok(());
		}

		// Older versions: a single ANNOUNCE_BROADCAST message, size-prefixed, with the
		// status carried inside the body.
		let mut body = Vec::new();
		match self {
			Self::Active { suffix, hops } => {
				AnnounceStatus::Active.encode(&mut body, version)?;
				suffix.encode(&mut body, version)?;
				encode_hops(&mut body, version, hops)?;
			}
			Self::Ended { suffix, hops } => {
				AnnounceStatus::Ended.encode(&mut body, version)?;
				suffix.encode(&mut body, version)?;
				encode_hops(&mut body, version, hops)?;
			}
			// The id-referencing forms only exist on lite-06+.
			Self::EndedId { .. } | Self::Restart { .. } => return Err(EncodeError::Version),
		}
		(body.len() as u64).encode(w, version)?;
		w.put_slice(&body);
		Ok(())
	}
}

impl Decode<Version> for AnnounceBroadcast<'_> {
	fn decode<B: Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		if version.has_announce_id() {
			// Lite06+: outer type, then a size-prefixed body decoded within its bounds.
			let typ = u64::decode(buf, version)?;
			let size = usize::decode(buf, version)?;
			if buf.remaining() < size {
				return Err(DecodeError::Short);
			}
			let mut body = buf.take(size);
			let msg = match typ {
				ANNOUNCE_START => Self::Active {
					suffix: Path::decode(&mut body, version)?,
					hops: OriginList::decode(&mut body, version)?,
				},
				ANNOUNCE_END => Self::EndedId {
					id: u64::decode(&mut body, version)?,
				},
				ANNOUNCE_RESTART => Self::Restart {
					id: u64::decode(&mut body, version)?,
					hops: OriginList::decode(&mut body, version)?,
				},
				_ => return Err(DecodeError::InvalidMessage(typ)),
			};
			if body.remaining() > 0 {
				return Err(DecodeError::Long);
			}
			return Ok(msg);
		}

		// Older versions: a single size-prefixed ANNOUNCE_BROADCAST with an inner status.
		let size = usize::decode(buf, version)?;
		if buf.remaining() < size {
			return Err(DecodeError::Short);
		}
		let mut body = buf.take(size);
		let msg = Self::decode_legacy(&mut body, version)?;
		if body.remaining() > 0 {
			return Err(DecodeError::Long);
		}
		Ok(msg)
	}
}

impl AnnounceBroadcast<'_> {
	/// Decode the body of a pre-lite-06 ANNOUNCE_BROADCAST (inner status + path + hops).
	fn decode_legacy<R: Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let status = AnnounceStatus::decode(r, version)?;

		let suffix = Path::decode(r, version)?;
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

		Ok(match status {
			AnnounceStatus::Active => Self::Active { suffix, hops },
			AnnounceStatus::Ended => Self::Ended { suffix, hops },
			// On lite-05 we encode a restart as a duplicate ANNOUNCE (a second `Active`), but we
			// also accept the draft's explicit `restart` status and treat it the same. For an
			// already-announced path the subscriber turns it into a restart; for an unknown path
			// it's a fresh announce. Older versions never defined this status, so it's an invalid
			// value there.
			AnnounceStatus::Restart if restart_supported(version) => Self::Active { suffix, hops },
			AnnounceStatus::Restart => return Err(DecodeError::InvalidValue),
		})
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
	/// The explicit restart status. Encoded on lite-06+ (referencing an announce id); on lite-05
	/// a restart goes out as a duplicate `Active` instead, but we still accept the explicit
	/// status on decode for forward/cross-compatibility.
	Restart = 2,
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
			AnnounceBroadcast::Active { suffix, hops } => AnnounceBroadcast::Active {
				suffix: suffix.to_owned(),
				hops,
			},
			AnnounceBroadcast::Ended { suffix, hops } => AnnounceBroadcast::Ended {
				suffix: suffix.to_owned(),
				hops,
			},
			AnnounceBroadcast::EndedId { id } => AnnounceBroadcast::EndedId { id },
			AnnounceBroadcast::Restart { id, hops } => AnnounceBroadcast::Restart { id, hops },
		}
	}

	#[test]
	fn announce_broadcast_round_trip_on_lite05() {
		let mut hops = OriginList::new();
		hops.push(Origin::new(7).unwrap()).unwrap();
		let msg = AnnounceBroadcast::Active {
			suffix: Path::new("room/cam"),
			hops: hops.clone(),
		};
		assert_eq!(broadcast_round_trip(&msg, Version::Lite05), msg);

		let ended = AnnounceBroadcast::Ended {
			suffix: Path::new("room/cam"),
			hops: OriginList::new(),
		};
		assert_eq!(broadcast_round_trip(&ended, Version::Lite05), ended);
	}

	#[test]
	fn announce_broadcast_round_trip_on_lite06() {
		let mut hops = OriginList::new();
		hops.push(Origin::new(7).unwrap()).unwrap();

		let active = AnnounceBroadcast::Active {
			suffix: Path::new("room/cam"),
			hops: hops.clone(),
		};
		assert_eq!(broadcast_round_trip(&active, Version::Lite06Wip), active);

		let ended = AnnounceBroadcast::EndedId { id: 3 };
		assert_eq!(broadcast_round_trip(&ended, Version::Lite06Wip), ended);

		let restart = AnnounceBroadcast::Restart { id: 3, hops };
		assert_eq!(broadcast_round_trip(&restart, Version::Lite06Wip), restart);
	}

	// The id-referencing forms don't exist before lite-06, and the path form is gone on lite-06.
	#[test]
	fn announce_broadcast_rejects_cross_version_forms() {
		let mut buf = bytes::BytesMut::new();
		assert!(matches!(
			AnnounceBroadcast::EndedId { id: 1 }.encode(&mut buf, Version::Lite05),
			Err(EncodeError::Version)
		));
		assert!(matches!(
			AnnounceBroadcast::Restart {
				id: 1,
				hops: OriginList::new()
			}
			.encode(&mut buf, Version::Lite05),
			Err(EncodeError::Version)
		));
		assert!(matches!(
			AnnounceBroadcast::Ended {
				suffix: Path::new("room/cam"),
				hops: OriginList::new()
			}
			.encode(&mut buf, Version::Lite06Wip),
			Err(EncodeError::Version)
		));
	}

	// An ANNOUNCE_END message on lite-06 is tiny: type byte, size prefix, id varint.
	#[test]
	fn ended_by_id_is_three_bytes() {
		let mut buf = bytes::BytesMut::new();
		AnnounceBroadcast::EndedId { id: 42 }
			.encode(&mut buf, Version::Lite06Wip)
			.unwrap();
		assert_eq!(buf.len(), 3);
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
}
