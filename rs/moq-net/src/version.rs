use std::fmt;
use std::str::FromStr;

use crate::{coding, ietf, lite};

/// The versions of MoQ that a session without a version ALPN (`moql`, or WebTransport
/// with no subprotocol) can negotiate in the bidi SETUP.
///
/// Ordered by preference, with the client's preference taking priority. Lite03 and newer
/// hand off (see [`Version::hands_off`]).
/// moq-transport negotiates only by ALPN.
#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
pub(crate) const NEGOTIATED: [Version; 7] = [
	Version::Lite(lite::Version::Lite07),
	Version::Lite(lite::Version::Lite06),
	Version::Lite(lite::Version::Lite05),
	Version::Lite(lite::Version::Lite04),
	Version::Lite(lite::Version::Lite03),
	Version::Lite(lite::Version::Lite02),
	Version::Lite(lite::Version::Lite01),
];

/// ALPN strings for supported versions, most-preferred first. `ALPNS[0]` is the
/// newest moq-lite ALPN that both sides converge on.
///
/// `ALPN_LITE_07_WIP` is deliberately absent: lite-07's wire format is still
/// work-in-progress, so it is never advertised or negotiated by default. It is only
/// reachable when both peers explicitly opt in (e.g. `--version moq-lite-07-wip`).
pub const ALPNS: &[&str] = &[
	ALPN_LITE_06,
	ALPN_LITE_05,
	ALPN_LITE_04,
	ALPN_LITE_03,
	ALPN_LITE,
	ALPN_22,
	ALPN_20,
	ALPN_18,
];

// ALPN constants
pub(crate) const ALPN_LITE: &str = "moql";
pub(crate) const ALPN_LITE_03: &str = "moq-lite-03";
pub(crate) const ALPN_LITE_04: &str = "moq-lite-04";
pub(crate) const ALPN_LITE_05: &str = "moq-lite-05";
pub(crate) const ALPN_LITE_06: &str = "moq-lite-06";
pub(crate) const ALPN_LITE_07_WIP: &str = "moq-lite-07-wip";
pub(crate) const ALPN_18: &str = "moqt-18";
pub(crate) const ALPN_20: &str = "moqt-20";
pub(crate) const ALPN_22: &str = "moqt-22";

const ALL: [Version; 10] = [
	Version::Lite(lite::Version::Lite07),
	Version::Lite(lite::Version::Lite06),
	Version::Lite(lite::Version::Lite05),
	Version::Lite(lite::Version::Lite04),
	Version::Lite(lite::Version::Lite03),
	Version::Lite(lite::Version::Lite02),
	Version::Lite(lite::Version::Lite01),
	Version::Ietf(ietf::Version::Draft22),
	Version::Ietf(ietf::Version::Draft20),
	Version::Ietf(ietf::Version::Draft18),
];

/// A MoQ protocol version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Version {
	/// A `moq-lite` draft, the simplified protocol this project specifies.
	Lite(lite::Version),
	/// An IETF `moq-transport` draft.
	Ietf(ietf::Version),
}

impl Version {
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn session_code(self, error: &crate::SessionError) -> u32 {
		match self {
			Self::Lite(_) => crate::lite::error::to_session_code(error),
			Self::Ietf(_) => crate::ietf::error::to_session_code(error),
		}
	}

	/// Whether the client's SETUP can carry its connection credential (see
	/// `Client::with_token`, behind the `async` feature). Every moq-transport draft can;
	/// moq-lite gained it in lite-07, so older versions need the credential in the URL.
	pub fn has_setup_token(self) -> bool {
		match self {
			Self::Lite(version) => version.has_auth(),
			Self::Ietf(_) => true,
		}
	}

	/// Handshake / config name, e.g. `moq-lite-05` or `moq-transport-22`.
	pub fn as_str(&self) -> &'static str {
		match self {
			Self::Lite(lite::Version::Lite01) => "moq-lite-01",
			Self::Lite(lite::Version::Lite02) => "moq-lite-02",
			Self::Lite(lite::Version::Lite03) => "moq-lite-03",
			Self::Lite(lite::Version::Lite04) => "moq-lite-04",
			Self::Lite(lite::Version::Lite05) => "moq-lite-05",
			Self::Lite(lite::Version::Lite06) => "moq-lite-06",
			Self::Lite(lite::Version::Lite07) => "moq-lite-07-wip",
			Self::Ietf(ietf::Version::Draft18) => "moq-transport-18",
			Self::Ietf(ietf::Version::Draft20) => "moq-transport-20",
			Self::Ietf(ietf::Version::Draft22) => "moq-transport-22",
		}
	}

	/// Parse from wire version code (used during SETUP negotiation).
	pub fn from_code(code: u64) -> Option<Self> {
		match code {
			0xff0dad01 => Some(Self::Lite(lite::Version::Lite01)),
			0xff0dad02 => Some(Self::Lite(lite::Version::Lite02)),
			0xff0dad03 => Some(Self::Lite(lite::Version::Lite03)),
			0xff0dad04 => Some(Self::Lite(lite::Version::Lite04)),
			0xff0dad05 => Some(Self::Lite(lite::Version::Lite05)),
			0xff0dad06 => Some(Self::Lite(lite::Version::Lite06)),
			0xff0dad07 => Some(Self::Lite(lite::Version::Lite07)),
			0xff000012 => Some(Self::Ietf(ietf::Version::Draft18)),
			0xff000014 => Some(Self::Ietf(ietf::Version::Draft20)),
			0xff000016 => Some(Self::Ietf(ietf::Version::Draft22)),
			_ => None,
		}
	}

	/// Get the wire version code.
	pub fn code(&self) -> u64 {
		match self {
			Self::Lite(lite::Version::Lite01) => 0xff0dad01,
			Self::Lite(lite::Version::Lite02) => 0xff0dad02,
			Self::Lite(lite::Version::Lite03) => 0xff0dad03,
			Self::Lite(lite::Version::Lite04) => 0xff0dad04,
			Self::Lite(lite::Version::Lite05) => 0xff0dad05,
			Self::Lite(lite::Version::Lite06) => 0xff0dad06,
			Self::Lite(lite::Version::Lite07) => 0xff0dad07,
			Self::Ietf(ietf::Version::Draft18) => 0xff000012,
			Self::Ietf(ietf::Version::Draft20) => 0xff000014,
			Self::Ietf(ietf::Version::Draft22) => 0xff000016,
		}
	}

	/// Returns the ALPN string for this version.
	pub fn alpn(&self) -> &'static str {
		match self {
			Self::Lite(lite::Version::Lite07) => ALPN_LITE_07_WIP,
			Self::Lite(lite::Version::Lite06) => ALPN_LITE_06,
			Self::Lite(lite::Version::Lite05) => ALPN_LITE_05,
			Self::Lite(lite::Version::Lite04) => ALPN_LITE_04,
			Self::Lite(lite::Version::Lite03) => ALPN_LITE_03,
			Self::Lite(lite::Version::Lite01 | lite::Version::Lite02) => ALPN_LITE,
			Self::Ietf(ietf::Version::Draft18) => ALPN_18,
			Self::Ietf(ietf::Version::Draft20) => ALPN_20,
			Self::Ietf(ietf::Version::Draft22) => ALPN_22,
		}
	}

	/// Whether SETUP selecting this version ends the SETUP stream, the session continuing
	/// as this version's ALPN would rather than on that stream.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn hands_off(self) -> bool {
		matches!(self, Self::Lite(v) if !matches!(v, lite::Version::Lite01 | lite::Version::Lite02))
	}

	/// Whether this is a lite protocol version.
	pub fn is_lite(&self) -> bool {
		match self {
			Self::Lite(_) => true,
			Self::Ietf(_) => false,
		}
	}
}

impl fmt::Display for Version {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl FromStr for Version {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		ALL.iter()
			.find(|version| version.as_str() == s)
			.copied()
			.ok_or_else(|| format!("unknown version: {s}"))
	}
}

impl serde::Serialize for Version {
	fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
		serializer.serialize_str(&self.to_string())
	}
}

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

impl<'a> coding::Decode<'a, Version> for Version {
	fn decode(r: &mut coding::Decoder<'_>, version: Version) -> Result<Self, coding::DecodeError> {
		coding::Version::decode(r, version).and_then(|v| v.try_into().map_err(|_| coding::DecodeError::InvalidValue))
	}
}

impl coding::Encode<Version> for Version {
	fn encode(&self, w: &mut coding::Encoder<'_>, v: Version) -> Result<(), coding::EncodeError> {
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
	/// All versions exposed by default.
	///
	/// `Lite07` is intentionally excluded: its wire format is still work-in-progress,
	/// so it is not advertised until a caller opts in explicitly (e.g. a pinned
	/// `version = ["moq-lite-07-wip"]`). An opt-in set that includes it negotiates normally.
	pub fn all() -> Self {
		Self(
			ALL.iter()
				.filter(|version| !matches!(version, Version::Lite(lite::Version::Lite07)))
				.copied()
				.collect(),
		)
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

	/// Returns `true` if the set includes this version.
	pub fn contains(&self, version: &Version) -> bool {
		self.0.contains(version)
	}

	/// Iterate the set in preference order, most preferred first.
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn default_versions_prefer_lite_06() {
		let newest = Version::Lite(lite::Version::Lite06);
		assert_eq!(newest.alpn(), "moq-lite-06");
		assert!("moq-lite-06-wip".parse::<Version>().is_err());
		assert_eq!(Versions::all().iter().next(), Some(&newest));
		assert_eq!(Versions::all().alpns().first(), Some(&newest.alpn()));
		assert_eq!(ALPNS.first(), Some(&newest.alpn()));
	}

	#[test]
	fn lite_07_wip_is_opt_in_only() {
		let wip = Version::Lite(lite::Version::Lite07);
		assert_eq!(wip.alpn(), "moq-lite-07-wip");
		assert_eq!(wip.to_string(), "moq-lite-07-wip");
		assert_eq!("moq-lite-07-wip".parse::<Version>(), Ok(wip));
		assert!("moq-lite-07".parse::<Version>().is_err());

		assert!(!Versions::all().contains(&wip));
		assert!(!ALPNS.contains(&wip.alpn()));
		assert_eq!(Versions::from(wip).alpns(), [wip.alpn()]);
	}
}
