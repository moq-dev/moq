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
pub const ALPNS: &[&str] = &[
	ALPN_LITE_04,
	ALPN_LITE_03,
	ALPN_LITE,
	ALPN_18,
	ALPN_17,
	ALPN_16,
	ALPN_15,
	ALPN_14,
];

// ALPN constants
pub(crate) const ALPN_LITE: &str = "moql";
pub(crate) const ALPN_LITE_03: &str = "moq-lite-03";
pub(crate) const ALPN_LITE_04: &str = "moq-lite-04";
pub(crate) const ALPN_14: &str = "moq-00";
pub(crate) const ALPN_15: &str = "moqt-15";
pub(crate) const ALPN_16: &str = "moqt-16";
pub(crate) const ALPN_17: &str = "moqt-17";
pub(crate) const ALPN_18: &str = "moqt-18";

/// The qmux draft version used to carry a MoQ ALPN over WebSocket / TLS.
///
/// The MoQ WG decided that qmux's version is tied to the moq-transport draft
/// (moq-transport-18 requires qmux-01; moq-transport-14..17 use qmux-00).
/// moq-lite is unconstrained and may ride on either.
///
/// Mirrors `qmux::Version` but kept local so `moq-net` stays independent of
/// the `qmux` crate; the `moq-native` layer converts at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum QmuxVersion {
	QMux00,
	QMux01,
}

impl QmuxVersion {
	/// The bare ALPN string for this qmux version.
	pub fn alpn(&self) -> &'static str {
		match self {
			Self::QMux00 => "qmux-00",
			Self::QMux01 => "qmux-01",
		}
	}
}

impl fmt::Display for QmuxVersion {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.alpn())
	}
}

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
			0xff0dad04 => Some(Self::Lite(lite::Version::Lite04)),
			0xff00000e => Some(Self::Ietf(ietf::Version::Draft14)),
			0xff00000f => Some(Self::Ietf(ietf::Version::Draft15)),
			0xff000010 => Some(Self::Ietf(ietf::Version::Draft16)),
			0xff000011 => Some(Self::Ietf(ietf::Version::Draft17)),
			0xff000012 => Some(Self::Ietf(ietf::Version::Draft18)),
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
			Self::Ietf(ietf::Version::Draft14) => 0xff00000e,
			Self::Ietf(ietf::Version::Draft15) => 0xff00000f,
			Self::Ietf(ietf::Version::Draft16) => 0xff000010,
			Self::Ietf(ietf::Version::Draft17) => 0xff000011,
			Self::Ietf(ietf::Version::Draft18) => 0xff000012,
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
			ALPN_LITE_04 => Some(Self::Lite(lite::Version::Lite04)),
			ALPN_14 => Some(Self::Ietf(ietf::Version::Draft14)),
			ALPN_15 => Some(Self::Ietf(ietf::Version::Draft15)),
			ALPN_16 => Some(Self::Ietf(ietf::Version::Draft16)),
			ALPN_17 => Some(Self::Ietf(ietf::Version::Draft17)),
			ALPN_18 => Some(Self::Ietf(ietf::Version::Draft18)),
			_ => None,
		}
	}

	/// Returns the ALPN string for this version.
	pub fn alpn(&self) -> &'static str {
		match self {
			Self::Lite(lite::Version::Lite04) => ALPN_LITE_04,
			Self::Lite(lite::Version::Lite03) => ALPN_LITE_03,
			Self::Lite(lite::Version::Lite01 | lite::Version::Lite02) => ALPN_LITE,
			Self::Ietf(ietf::Version::Draft14) => ALPN_14,
			Self::Ietf(ietf::Version::Draft15) => ALPN_15,
			Self::Ietf(ietf::Version::Draft16) => ALPN_16,
			Self::Ietf(ietf::Version::Draft17) => ALPN_17,
			Self::Ietf(ietf::Version::Draft18) => ALPN_18,
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

	/// The qmux versions this MoQ version may ride on, in preference order.
	///
	/// moq-transport-18 requires qmux-01; moq-transport-14..17 require qmux-00.
	/// Existing moq-lite versions (Lite01..Lite04) advertise both for back-compat.
	/// Future moq-lite versions should pin to a single qmux version, like moq-transport.
	pub fn qmux_versions(&self) -> &'static [QmuxVersion] {
		use ietf::Version as I;
		use lite::Version as L;
		match self {
			Self::Ietf(I::Draft18) => &[QmuxVersion::QMux01],
			Self::Ietf(I::Draft14 | I::Draft15 | I::Draft16 | I::Draft17) => &[QmuxVersion::QMux00],
			Self::Lite(L::Lite01 | L::Lite02 | L::Lite03 | L::Lite04) => {
				&[QmuxVersion::QMux01, QmuxVersion::QMux00]
			}
		}
	}

	/// Whether this MoQ version is permitted to ride on the given qmux version.
	///
	/// Use server-side after the qmux/app pair has been negotiated to reject
	/// pairings the moq-transport spec forbids (e.g. `qmux-00.moqt-18`).
	pub fn accepts_qmux(&self, qv: QmuxVersion) -> bool {
		self.qmux_versions().contains(&qv)
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
			"moq-lite-04" => Ok(Self::Lite(lite::Version::Lite04)),
			"moq-transport-14" => Ok(Self::Ietf(ietf::Version::Draft14)),
			"moq-transport-15" => Ok(Self::Ietf(ietf::Version::Draft15)),
			"moq-transport-16" => Ok(Self::Ietf(ietf::Version::Draft16)),
			"moq-transport-17" => Ok(Self::Ietf(ietf::Version::Draft17)),
			"moq-transport-18" => Ok(Self::Ietf(ietf::Version::Draft18)),
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
			Version::Lite(lite::Version::Lite04),
			Version::Lite(lite::Version::Lite03),
			Version::Lite(lite::Version::Lite02),
			Version::Lite(lite::Version::Lite01),
			Version::Ietf(ietf::Version::Draft18),
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

	/// Compute the `(qmux_version, app_alpn)` pairs to advertise over WebSocket / TLS,
	/// in preference order, dedup'd.
	///
	/// Each MoQ version is paired only with the qmux versions it's permitted to ride on
	/// (see [`Version::qmux_versions`]). Use this to build the `Sec-WebSocket-Protocol`
	/// list (or TLS ALPN list) when fronting a qmux session.
	pub fn qmux_alpns(&self) -> Vec<(QmuxVersion, &'static str)> {
		let mut pairs = Vec::new();
		for v in &self.0 {
			let alpn = v.alpn();
			for &qv in v.qmux_versions() {
				let pair = (qv, alpn);
				if !pairs.contains(&pair) {
					pairs.push(pair);
				}
			}
		}
		pairs
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn qmux_versions_for_each_moq_version() {
		assert_eq!(
			Version::Ietf(ietf::Version::Draft18).qmux_versions(),
			&[QmuxVersion::QMux01]
		);
		for v in [
			ietf::Version::Draft14,
			ietf::Version::Draft15,
			ietf::Version::Draft16,
			ietf::Version::Draft17,
		] {
			assert_eq!(Version::Ietf(v).qmux_versions(), &[QmuxVersion::QMux00], "{v}");
		}
		for v in [
			lite::Version::Lite01,
			lite::Version::Lite02,
			lite::Version::Lite03,
			lite::Version::Lite04,
		] {
			assert_eq!(
				Version::Lite(v).qmux_versions(),
				&[QmuxVersion::QMux01, QmuxVersion::QMux00],
				"{v}"
			);
		}
	}

	#[test]
	fn accepts_qmux_is_consistent() {
		assert!(Version::Ietf(ietf::Version::Draft18).accepts_qmux(QmuxVersion::QMux01));
		assert!(!Version::Ietf(ietf::Version::Draft18).accepts_qmux(QmuxVersion::QMux00));
		assert!(Version::Ietf(ietf::Version::Draft17).accepts_qmux(QmuxVersion::QMux00));
		assert!(!Version::Ietf(ietf::Version::Draft17).accepts_qmux(QmuxVersion::QMux01));
		assert!(Version::Lite(lite::Version::Lite04).accepts_qmux(QmuxVersion::QMux01));
		assert!(Version::Lite(lite::Version::Lite04).accepts_qmux(QmuxVersion::QMux00));
	}

	#[test]
	fn qmux_alpns_all_matches_table() {
		let pairs = Versions::all().qmux_alpns();
		assert_eq!(
			pairs,
			vec![
				(QmuxVersion::QMux01, "moq-lite-04"),
				(QmuxVersion::QMux00, "moq-lite-04"),
				(QmuxVersion::QMux01, "moq-lite-03"),
				(QmuxVersion::QMux00, "moq-lite-03"),
				(QmuxVersion::QMux01, "moql"),
				(QmuxVersion::QMux00, "moql"),
				(QmuxVersion::QMux01, "moqt-18"),
				(QmuxVersion::QMux00, "moqt-17"),
				(QmuxVersion::QMux00, "moqt-16"),
				(QmuxVersion::QMux00, "moqt-15"),
				(QmuxVersion::QMux00, "moq-00"),
			]
		);
	}

	#[test]
	fn qmux_alpns_singleton_moqt_18() {
		assert_eq!(
			Versions::from(Version::Ietf(ietf::Version::Draft18)).qmux_alpns(),
			vec![(QmuxVersion::QMux01, "moqt-18")]
		);
	}

	#[test]
	fn qmux_alpns_singleton_moqt_17() {
		assert_eq!(
			Versions::from(Version::Ietf(ietf::Version::Draft17)).qmux_alpns(),
			vec![(QmuxVersion::QMux00, "moqt-17")]
		);
	}

	#[test]
	fn qmux_alpns_singleton_lite_offers_both() {
		assert_eq!(
			Versions::from(Version::Lite(lite::Version::Lite04)).qmux_alpns(),
			vec![
				(QmuxVersion::QMux01, "moq-lite-04"),
				(QmuxVersion::QMux00, "moq-lite-04"),
			]
		);
	}

	#[test]
	fn qmux_version_alpn_strings() {
		assert_eq!(QmuxVersion::QMux00.alpn(), "qmux-00");
		assert_eq!(QmuxVersion::QMux01.alpn(), "qmux-01");
	}
}
