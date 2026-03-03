use std::fmt;

/// An IETF protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Draft14,
	Draft15,
	Draft16,
	Draft17,
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Draft14 => write!(f, "moq-transport-14"),
			Self::Draft15 => write!(f, "moq-transport-15"),
			Self::Draft16 => write!(f, "moq-transport-16"),
			Self::Draft17 => write!(f, "moq-transport-17"),
		}
	}
}

impl From<Version> for crate::Version {
	fn from(v: Version) -> Self {
		match v {
			Version::Draft14 => crate::Version::Ietf(Version::Draft14),
			Version::Draft15 => crate::Version::Ietf(Version::Draft15),
			Version::Draft16 => crate::Version::Ietf(Version::Draft16),
			Version::Draft17 => crate::Version::Ietf(Version::Draft17),
		}
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
