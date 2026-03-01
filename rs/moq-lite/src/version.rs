use std::fmt;
use std::str::FromStr;

use crate::{coding, ietf, lite};

/// The versions of MoQ that are negotiated via SETUP.
///
/// Ordered by preference, with the client's preference taking priority.
pub(crate) const NEGOTIATED: [Version; 3] = [
	Version::Lite(lite::Version::Draft02),
	Version::Lite(lite::Version::Draft01),
	Version::Ietf(ietf::Version::Draft14),
];

/// ALPN strings for supported versions.
pub const ALPNS: &[&str] = &[lite::ALPN_03, lite::ALPN, ietf::ALPN_16, ietf::ALPN_15, ietf::ALPN_14];

/// A MoQ protocol version, combining both IETF and Lite variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Version {
	Ietf(ietf::Version),
	Lite(lite::Version),
}

impl Version {
	/// Returns the ALPN string for this version.
	pub fn alpn(&self) -> &'static str {
		match self {
			Self::Lite(lite::Version::Draft03) => lite::ALPN_03,
			Self::Lite(lite::Version::Draft01 | lite::Version::Draft02) => lite::ALPN,
			Self::Ietf(ietf::Version::Draft14) => ietf::ALPN_14,
			Self::Ietf(ietf::Version::Draft15) => ietf::ALPN_15,
			Self::Ietf(ietf::Version::Draft16) => ietf::ALPN_16,
		}
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Lite(lite::Version::Draft01) => write!(f, "moq-lite-01"),
			Self::Lite(lite::Version::Draft02) => write!(f, "moq-lite-02"),
			Self::Lite(lite::Version::Draft03) => write!(f, "moq-lite-03"),
			Self::Ietf(ietf::Version::Draft14) => write!(f, "moq-transport-14"),
			Self::Ietf(ietf::Version::Draft15) => write!(f, "moq-transport-15"),
			Self::Ietf(ietf::Version::Draft16) => write!(f, "moq-transport-16"),
		}
	}
}

impl FromStr for Version {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"moq-lite-01" => Ok(Self::Lite(lite::Version::Draft01)),
			"moq-lite-02" => Ok(Self::Lite(lite::Version::Draft02)),
			"moq-lite-03" => Ok(Self::Lite(lite::Version::Draft03)),
			"moq-transport-14" => Ok(Self::Ietf(ietf::Version::Draft14)),
			"moq-transport-15" => Ok(Self::Ietf(ietf::Version::Draft15)),
			"moq-transport-16" => Ok(Self::Ietf(ietf::Version::Draft16)),
			_ => Err(format!("unknown version: {s}")),
		}
	}
}

#[cfg(feature = "serde")]
impl serde::Serialize for Version {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(&self.to_string())
	}
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for Version {
	fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
		let s = String::deserialize(deserializer)?;
		s.parse().map_err(serde::de::Error::custom)
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
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: V) -> Result<(), coding::EncodeError> {
		match self {
			Self::Ietf(version) => coding::Version::from(*version).encode(w, v)?,
			Self::Lite(version) => coding::Version::from(*version).encode(w, v)?,
		}
		Ok(())
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

/// A set of supported MoQ versions.
#[derive(Debug, Clone)]
pub struct Versions(Vec<Version>);

impl Versions {
	/// All supported versions.
	pub fn all() -> Self {
		Self(vec![
			Version::Lite(lite::Version::Draft03),
			Version::Lite(lite::Version::Draft02),
			Version::Lite(lite::Version::Draft01),
			Version::Ietf(ietf::Version::Draft16),
			Version::Ietf(ietf::Version::Draft15),
			Version::Ietf(ietf::Version::Draft14),
		])
	}

	/// Compute the unique ALPN strings needed for these versions.
	pub fn alpns(&self) -> Vec<&'static str> {
		let mut alpns = Vec::new();
		for v in &self.0 {
			let alpn = v.alpn();
			if !alpns.contains(&alpn) {
				alpns.push(alpn);
			}
		}
		alpns
	}

	/// Return only versions present in both self and other, or `None` if the intersection is empty.
	pub fn filter(&self, other: &Versions) -> Option<Versions> {
		let filtered: Vec<Version> = self.0.iter().filter(|v| other.0.contains(v)).copied().collect();
		if filtered.is_empty() {
			None
		} else {
			Some(Versions(filtered))
		}
	}

	/// Check if a specific version is in this set.
	pub fn select(&self, version: Version) -> Option<Version> {
		self.0.contains(&version).then_some(version)
	}

	pub fn contains(&self, version: &Version) -> bool {
		self.0.contains(version)
	}

	pub fn iter(&self) -> impl Iterator<Item = &Version> {
		self.0.iter()
	}
}

impl Default for Versions {
	fn default() -> Self {
		Self::all()
	}
}

impl From<Version> for Versions {
	fn from(value: Version) -> Self {
		Self(vec![value])
	}
}

impl From<Vec<Version>> for Versions {
	fn from(value: Vec<Version>) -> Self {
		Self(value)
	}
}

impl<const N: usize> From<[Version; N]> for Versions {
	fn from(value: [Version; N]) -> Self {
		Self(value.to_vec())
	}
}

impl From<Versions> for coding::Versions {
	fn from(value: Versions) -> Self {
		let inner: Vec<coding::Version> = value.0.into_iter().map(|v| v.into()).collect();
		coding::Versions::from(inner)
	}
}
