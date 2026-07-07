use std::fmt;

/// A lite protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Lite01,
	Lite02,
	Lite03,
	Lite04,
	/// lite-05. Adds the TRACK stream (immutable per-track properties incl.
	/// timescale), zigzag-delta timestamps in per-frame headers, and drops
	/// SUBSCRIBE_OK/FETCH_OK. Advertised over ALPN and the preferred version in the
	/// default version sets.
	Lite05,
}

impl Version {
	/// Whether the version has lite-05's dedicated TRACK stream and related stream
	/// layout changes.
	///
	/// This is the common feature boundary for TRACK_INFO, FETCH streams,
	/// SUBSCRIBE_START/END, and per-frame timestamp prefixes.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_track_stream(self) -> bool {
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

	/// Whether the session may deliver groups over unreliable QUIC datagrams (lite-05 §6.4).
	/// A datagram carries one single-frame group's `subscribe | sequence | timestamp | payload`
	/// and is routed over the existing subscription. Added in lite-05; older versions never
	/// send or accept datagram bodies.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_datagrams(self) -> bool {
		// Match form so future versions default forward (CLAUDE.md convention).
		match self {
			Self::Lite01 | Self::Lite02 | Self::Lite03 | Self::Lite04 => false,
			_ => true,
		}
	}

	/// Whether announce streams begin with ANNOUNCE_OK and omit the sender's origin
	/// from each announcement's hop chain. Added in lite-05.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_announce_ok(self) -> bool {
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
			Self::Lite05 => write!(f, "moq-lite-05"),
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
			Version::Lite05 => crate::Version::Lite(Version::Lite05),
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
