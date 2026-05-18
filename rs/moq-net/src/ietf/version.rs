use std::fmt;

/// An IETF protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Draft14,
	Draft15,
	Draft16,
	Draft17,
	Draft18,
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Draft14 => write!(f, "moq-transport-14"),
			Self::Draft15 => write!(f, "moq-transport-15"),
			Self::Draft16 => write!(f, "moq-transport-16"),
			Self::Draft17 => write!(f, "moq-transport-17"),
			Self::Draft18 => write!(f, "moq-transport-18"),
		}
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
