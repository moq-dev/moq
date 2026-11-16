use std::fmt;

/// An IETF protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Version {
	Draft18,
	Draft22,
	Draft20,
	Draft23,
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Draft18 => write!(f, "moq-transport-18"),
			Self::Draft20 => write!(f, "moq-transport-20"),
			Self::Draft22 => write!(f, "moq-transport-22"),
			Self::Draft23 => write!(f, "moq-transport-23"),
		}
	}
}

impl Version {
	/// Whether a control message's Message Length is a varint (draft-23 section 9) rather
	/// than the 16-bit length of every earlier draft.
	pub(crate) fn varint_length(self) -> bool {
		!matches!(self, Self::Draft18 | Self::Draft20 | Self::Draft22)
	}

	/// Whether the PAUSED parameter (0x11) replaces FORWARD (0x10), with the opposite sense
	/// and a default of 0, unpaused (draft-23 section 9.19.18).
	pub(crate) fn paused(self) -> bool {
		!matches!(self, Self::Draft18 | Self::Draft20 | Self::Draft22)
	}

	/// The Forward State a message carries: FORWARD as sent, or PAUSED inverted, whichever
	/// `self` puts on the wire. `None` when the message carried neither.
	pub(crate) fn forward_state(forward: Option<bool>, paused: Option<bool>) -> Option<bool> {
		forward.or(paused.map(|paused| !paused))
	}
}

impl From<Version> for crate::Version {
	fn from(v: Version) -> Self {
		crate::Version::Ietf(v)
	}
}

impl TryFrom<crate::Version> for Version {
	type Error = ();

	fn try_from(v: crate::Version) -> Result<Self, Self::Error> {
		match v {
			crate::Version::Ietf(v) => Ok(v),
			crate::Version::Lite(_) => Err(()),
		}
	}
}
