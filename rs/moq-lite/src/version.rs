use std::fmt;
use std::str::FromStr;

use crate::{coding, ietf, lite};

/// The versions of MoQ that are negotiated via SETUP.
///
/// Ordered by preference, with the client's preference taking priority.
/// This intentionally includes only SETUP-negotiated versions (Lite02, Lite01, Draft14);
/// Lite03 and newer IETF drafts negotiate via dedicated ALPNs instead.
pub(crate) const NEGOTIATED: [Version; 3] = [
	Version::Lite(lite::Version::Lite02),
	Version::Lite(lite::Version::Lite01),
	Version::Ietf(ietf::Version::Draft14),
];

/// ALPN strings for supported versions.
pub const ALPNS: &[&str] = &[ALPN_LITE_03, ALPN_LITE, ALPN_17, ALPN_16, ALPN_15, ALPN_14];

// ALPN constants
pub const ALPN_LITE: &str = "moql";
pub const ALPN_LITE_03: &str = "moq-lite-03";
pub const ALPN_14: &str = "moq-00";
pub const ALPN_15: &str = "moqt-15";
pub const ALPN_16: &str = "moqt-16";
pub const ALPN_17: &str = "moqt-17";

/// A MoQ protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Version {
	Lite(lite::Version),
	Ietf(ietf::Version),
}

impl Version {
	/// Parse from wire version code (used during SETUP negotiation).
	pub fn from_code(code: u64) -> Option<Self> {
		match code {
			0xff0dad01 => Some(Self::Lite(lite::Version::Lite01)),
			0xff0dad02 => Some(Self::Lite(lite::Version::Lite02)),
			0xff0dad03 => Some(Self::Lite(lite::Version::Lite03)),
			0xff00000e => Some(Self::Ietf(ietf::Version::Draft14)),
			0xff00000f => Some(Self::Ietf(ietf::Version::Draft15)),
			0xff000010 => Some(Self::Ietf(ietf::Version::Draft16)),
			0xff000011 => Some(Self::Ietf(ietf::Version::Draft17)),
			_ => None,
		}
	}

	/// Get the wire version code.
	pub fn code(&self) -> u64 {
		match self {
			Self::Lite(lite::Version::Lite01) => 0xff0dad01,
			Self::Lite(lite::Version::Lite02) => 0xff0dad02,
			Self::Lite(lite::Version::Lite03) => 0xff0dad03,
			Self::Ietf(ietf::Version::Draft14) => 0xff00000e,
			Self::Ietf(ietf::Version::Draft15) => 0xff00000f,
			Self::Ietf(ietf::Version::Draft16) => 0xff000010,
			Self::Ietf(ietf::Version::Draft17) => 0xff000011,
		}
	}

	/// Parse from ALPN string.
	///
	/// Returns `None` for `ALPN_LITE` since multiple versions share
	/// that ALPN, requiring SETUP negotiation to determine the version.
	pub fn from_alpn(alpn: &str) -> Option<Self> {
		match alpn {
			ALPN_LITE => None, // Multiple versions share this ALPN, need SETUP negotiation
			ALPN_LITE_03 => Some(Self::Lite(lite::Version::Lite03)),
			ALPN_14 => Some(Self::Ietf(ietf::Version::Draft14)),
			ALPN_15 => Some(Self::Ietf(ietf::Version::Draft15)),
			ALPN_16 => Some(Self::Ietf(ietf::Version::Draft16)),
			ALPN_17 => Some(Self::Ietf(ietf::Version::Draft17)),
			_ => None,
		}
	}

	/// Returns the ALPN string for this version.
	pub fn alpn(&self) -> &'static str {
		match self {
			Self::Lite(lite::Version::Lite03) => ALPN_LITE_03,
			Self::Lite(lite::Version::Lite01 | lite::Version::Lite02) => ALPN_LITE,
			Self::Ietf(ietf::Version::Draft14) => ALPN_14,
			Self::Ietf(ietf::Version::Draft15) => ALPN_15,
			Self::Ietf(ietf::Version::Draft16) => ALPN_16,
			Self::Ietf(ietf::Version::Draft17) => ALPN_17,
		}
	}

	/// Whether this version uses SETUP version-code negotiation
	/// (as opposed to ALPN-only).
	pub fn uses_setup_negotiation(&self) -> bool {
		matches!(
			self,
			Self::Lite(lite::Version::Lite01 | lite::Version::Lite02) | Self::Ietf(ietf::Version::Draft14)
		)
	}

	/// Whether this is a lite protocol version.
	pub fn is_lite(&self) -> bool {
		match self {
			Self::Lite(_) => true,
			Self::Ietf(_) => false,
		}
	}

	/// Whether this is an IETF protocol version.
	pub fn is_ietf(&self) -> bool {
		match self {
			Self::Ietf(_) => true,
			Self::Lite(_) => false,
		}
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Lite(v) => v.fmt(f),
			Self::Ietf(v) => v.fmt(f),
		}
	}
}

impl FromStr for Version {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"moq-lite-01" => Ok(Self::Lite(lite::Version::Lite01)),
			"moq-lite-02" => Ok(Self::Lite(lite::Version::Lite02)),
			"moq-lite-03" => Ok(Self::Lite(lite::Version::Lite03)),
			"moq-transport-14" => Ok(Self::Ietf(ietf::Version::Draft14)),
			"moq-transport-15" => Ok(Self::Ietf(ietf::Version::Draft15)),
			"moq-transport-16" => Ok(Self::Ietf(ietf::Version::Draft16)),
			"moq-transport-17" => Ok(Self::Ietf(ietf::Version::Draft17)),
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

impl TryFrom<coding::Version> for Version {
	type Error = ();

	fn try_from(value: coding::Version) -> Result<Self, Self::Error> {
		Self::from_code(value.0).ok_or(())
	}
}

impl coding::Decode<Version> for Version {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, coding::DecodeError> {
		coding::Version::decode(r, version).and_then(|v| v.try_into().map_err(|_| coding::DecodeError::InvalidValue))
	}
}

impl coding::Encode<Version> for Version {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, v: Version) -> Result<(), coding::EncodeError> {
		coding::Version::from(*self).encode(w, v)
	}
}

impl From<Version> for coding::Version {
	fn from(value: Version) -> Self {
		Self(value.code())
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
	/// All supported versions exposed by default.
	pub fn all() -> Self {
		Self(vec![
			Version::Lite(lite::Version::Lite03),
			Version::Lite(lite::Version::Lite02),
			Version::Lite(lite::Version::Lite01),
			Version::Ietf(ietf::Version::Draft17),
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
