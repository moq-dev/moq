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
