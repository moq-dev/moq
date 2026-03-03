use std::fmt;
use std::str::FromStr;

use crate::coding;

/// The versions of MoQ that are negotiated via SETUP.
///
/// Ordered by preference, with the client's preference taking priority.
/// This intentionally includes only SETUP-negotiated versions (Lite01, Lite02, Draft14);
/// Lite03 and newer IETF drafts negotiate via dedicated ALPNs instead.
pub(crate) const NEGOTIATED: [Version; 3] = [Version::Lite02, Version::Lite01, Version::Draft14];

/// ALPN strings for supported versions.
// NOTE: ALPN_17 intentionally excluded until draft-17 support is complete.
pub const ALPNS: &[&str] = &[ALPN_LITE_03, ALPN_LITE, ALPN_16, ALPN_15, ALPN_14];

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
#[repr(u64)]
pub enum Version {
	Lite01 = 0xff0dad01,
	Lite02 = 0xff0dad02,
	Lite03 = 0xff0dad03,
	Draft14 = 0xff00000e,
	Draft15 = 0xff00000f,
	Draft16 = 0xff000010,
	Draft17 = 0xff000011,
}

impl Version {
	/// Parse from wire version code (used during SETUP negotiation).
	pub fn from_code(code: u64) -> Option<Self> {
		match code {
			0xff0dad01 => Some(Self::Lite01),
			0xff0dad02 => Some(Self::Lite02),
			0xff0dad03 => Some(Self::Lite03),
			0xff00000e => Some(Self::Draft14),
			0xff00000f => Some(Self::Draft15),
			0xff000010 => Some(Self::Draft16),
			0xff000011 => Some(Self::Draft17),
			_ => None,
		}
	}

	/// Get the wire version code.
	pub fn code(&self) -> u64 {
		*self as u64
	}

	/// Parse from ALPN string.
	pub fn from_alpn(alpn: &str) -> Option<Self> {
		match alpn {
			ALPN_LITE => None, // Multiple versions share this ALPN, need SETUP negotiation
			ALPN_LITE_03 => Some(Self::Lite03),
			ALPN_14 => Some(Self::Draft14),
			ALPN_15 => Some(Self::Draft15),
			ALPN_16 => Some(Self::Draft16),
			ALPN_17 => Some(Self::Draft17),
			_ => None,
		}
	}

	/// Returns the ALPN string for this version.
	pub fn alpn(&self) -> &'static str {
		match self {
			Self::Lite03 => ALPN_LITE_03,
			Self::Lite01 | Self::Lite02 => ALPN_LITE,
			Self::Draft14 => ALPN_14,
			Self::Draft15 => ALPN_15,
			Self::Draft16 => ALPN_16,
			Self::Draft17 => ALPN_17,
		}
	}

	/// Whether this version uses SETUP version-code negotiation
	/// (as opposed to ALPN-only).
	pub fn uses_setup_negotiation(&self) -> bool {
		matches!(self, Self::Lite01 | Self::Lite02 | Self::Draft14)
	}

	/// Whether this is a lite protocol version.
	pub fn is_lite(&self) -> bool {
		matches!(self, Self::Lite01 | Self::Lite02 | Self::Lite03)
	}

	/// Whether this is an IETF protocol version.
	pub fn is_ietf(&self) -> bool {
		matches!(self, Self::Draft14 | Self::Draft15 | Self::Draft16 | Self::Draft17)
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Lite01 => write!(f, "moq-lite-01"),
			Self::Lite02 => write!(f, "moq-lite-02"),
			Self::Lite03 => write!(f, "moq-lite-03"),
			Self::Draft14 => write!(f, "moq-transport-14"),
			Self::Draft15 => write!(f, "moq-transport-15"),
			Self::Draft16 => write!(f, "moq-transport-16"),
			Self::Draft17 => write!(f, "moq-transport-17"),
		}
	}
}

impl FromStr for Version {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"moq-lite-01" => Ok(Self::Lite01),
			"moq-lite-02" => Ok(Self::Lite02),
			"moq-lite-03" => Ok(Self::Lite03),
			"moq-transport-14" => Ok(Self::Draft14),
			"moq-transport-15" => Ok(Self::Draft15),
			"moq-transport-16" => Ok(Self::Draft16),
			"moq-transport-17" => Ok(Self::Draft17),
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
	///
	/// This list intentionally omits Draft17 while draft-17 support remains incomplete.
	pub fn all() -> Self {
		Self(vec![
			Version::Lite03,
			Version::Lite02,
			Version::Lite01,
			Version::Draft16,
			Version::Draft15,
			Version::Draft14,
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
