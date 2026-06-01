use std::fmt;

/// A lite protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Lite01,
	Lite02,
	Lite03,
	Lite04,
	/// Work-in-progress placeholder for lite-05. Adds per-track timescale to
	/// SUBSCRIBE_OK and zigzag-delta timestamps to per-frame headers. Not
	/// advertised over ALPN or included in default version sets; callers must
	/// opt in explicitly.
	Lite05Wip,
}

impl Version {
	/// Whether SUBSCRIBE_OK can carry a per-track timescale. When the publisher
	/// advertises one, the publisher and subscriber agree to prefix every frame
	/// with a zigzag-delta timestamp varint; with `None` the wire skips the
	/// byte entirely, so this method only governs whether the negotiation field
	/// exists, not whether timestamps are always present.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_timestamps(self) -> bool {
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
