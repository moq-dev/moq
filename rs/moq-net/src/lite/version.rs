use std::fmt;

/// A lite protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Lite01,
	Lite02,
	Lite03,
	Lite04,
	/// Lite05 adds per-track timescale to SUBSCRIBE_OK and zigzag-delta timestamps
	/// to per-frame headers.
	Lite05,
}

impl Version {
	/// Whether this version carries per-frame timestamps and per-track timescale
	/// on the wire.
	#[allow(clippy::match_like_matches_macro)]
	pub fn has_timestamps(self) -> bool {
		// Match form is used so future versions default forward (CLAUDE.md convention).
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
		crate::Version::Lite(v)
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
