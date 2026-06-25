use std::fmt;

/// A lite protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Lite01,
	Lite02,
	Lite03,
	Lite04,
	/// Work-in-progress lite-05. Adds the TRACK stream (immutable per-track
	/// properties incl. timescale), zigzag-delta timestamps in per-frame headers,
	/// and drops SUBSCRIBE_OK/FETCH_OK. Advertised over ALPN and included in the
	/// default version sets as the preferred version; still WIP, revisit before
	/// promoting the branch to `main`.
	Lite05Wip,
}

impl Version {
	/// Whether the track can carry a per-track timescale (reported in TRACK_INFO on
	/// lite-05+). When the publisher advertises one, the publisher and subscriber
	/// agree to prefix every frame with a zigzag-delta timestamp varint; with `None`
	/// the wire skips the byte entirely, so this method only governs whether the
	/// negotiation field exists, not whether timestamps are always present.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_timestamps(self) -> bool {
		// Match form so future versions default forward (CLAUDE.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}

	/// Whether ANNOUNCE_BROADCAST carries a per-broadcast Epoch varint (after the
	/// suffix, before the hop chain). Added in lite-05 so a consumer can tell a newer
	/// instance of a broadcast from an older one. Older versions omit the field.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_broadcast_epoch(self) -> bool {
		// Match form so future versions default forward (CLAUDE.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}

	/// Whether the session opens a unidirectional Setup Stream carrying a single SETUP
	/// message (capabilities + optional Path). Added in lite-05; the older bidirectional
	/// setup exchange (Lite01/02) and the no-setup drafts (Lite03/04) don't use it.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_setup_stream(self) -> bool {
		// Match form so future versions default forward (CLAUDE.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Lite01 => write!(f, "moq-lite-01"),
			Self::Lite02 => write!(f, "moq-lite-02"),
			Self::Lite03 => write!(f, "moq-lite-03"),
			Self::Lite04 => write!(f, "moq-lite-04"),
			Self::Lite05Wip => write!(f, "moq-lite-05-wip"),
		}
	}
}

impl From<Version> for crate::Version {
	fn from(v: Version) -> Self {
		match v {
			Version::Lite01 => crate::Version::Lite(Version::Lite01),
			Version::Lite02 => crate::Version::Lite(Version::Lite02),
			Version::Lite03 => crate::Version::Lite(Version::Lite03),
			Version::Lite04 => crate::Version::Lite(Version::Lite04),
			Version::Lite05Wip => crate::Version::Lite(Version::Lite05Wip),
		}
	}
}

impl TryFrom<crate::Version> for Version {
	type Error = ();

	fn try_from(v: crate::Version) -> Result<Self, Self::Error> {
		match v {
			crate::Version::Lite(v) => Ok(v),
			crate::Version::Ietf(_) => Err(()),
		}
	}
}
