use crate::{coding, ietf, lite};

/// The versions of MoQ that are negotiated.
///
/// Ordered by preference, with the client's preference taking priority.
pub(crate) const NEGOTIATED: [Version; 3] = [
	Version::Lite(lite::Version::Draft02),
	Version::Lite(lite::Version::Draft01),
	Version::Ietf(ietf::Version::Draft14),
];

/// The ALPN strings for supported versions.
const ALPNS: [&str; 3] = [lite::ALPN, ietf::ALPN_14, ietf::ALPN_15];

// Return the ALPN strings for supported versions.
// This is a function so we can avoid semver bumps.
pub fn alpns() -> &'static [&'static str] {
	&ALPNS
}

// A combination of ietf::Version and lite::Version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Version {
	Ietf(ietf::Version),
	Lite(lite::Version),
}

impl Version {
	pub fn is_ietf(self) -> bool {
		matches!(self, Self::Ietf(_))
	}

	pub fn is_lite(self) -> bool {
		matches!(self, Self::Lite(_))
	}
}

impl From<ietf::Version> for Version {
	fn from(value: ietf::Version) -> Self {
		Self::Ietf(value)
	}
}

impl From<lite::Version> for Version {
	fn from(value: lite::Version) -> Self {
		Self::Lite(value)
	}
}

impl TryFrom<coding::Version> for Version {
	type Error = ();

	fn try_from(value: coding::Version) -> Result<Self, Self::Error> {
		ietf::Version::try_from(value)
			.map(Self::Ietf)
			.or_else(|_| lite::Version::try_from(value).map(Self::Lite))
	}
}

impl<V> coding::Decode<V> for Version {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, coding::DecodeError> {
		coding::Version::decode(r, version).and_then(|v| v.try_into().map_err(|_| coding::DecodeError::InvalidValue))
	}
}

impl<V> coding::Encode<V> for Version {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: V) {
		match self {
			Self::Ietf(version) => coding::Version::from(*version).encode(w, v),
			Self::Lite(version) => coding::Version::from(*version).encode(w, v),
		}
	}
}

impl From<Version> for coding::Version {
	fn from(value: Version) -> Self {
		match value {
			Version::Ietf(version) => version.into(),
			Version::Lite(version) => version.into(),
		}
	}
}

impl From<Vec<Version>> for coding::Versions {
	fn from(value: Vec<Version>) -> Self {
		let inner: Vec<coding::Version> = value.into_iter().map(|v| v.into()).collect();
		coding::Versions::from(inner)
	}
}
