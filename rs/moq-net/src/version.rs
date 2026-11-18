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
	Version::lite(lite::Version::Lite07),
	Version::lite(lite::Version::Lite06),
	Version::lite(lite::Version::Lite05),
	Version::lite(lite::Version::Lite04),
	Version::lite(lite::Version::Lite03),
	Version::lite(lite::Version::Lite02),
	Version::lite(lite::Version::Lite01),
];

/// ALPN strings for supported versions, most-preferred first. `ALPNS[0]` is the
/// newest moq-lite ALPN that both sides converge on.
///
/// `ALPN_23` is absent for the same reason as lite-07: the draft-23 port is a spike, so it
/// is only offered by a caller that names `moq-transport-23`.
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
pub(crate) const ALPN_23: &str = "moqt-23";

const ALL: [Version; 11] = [
	Version::lite(lite::Version::Lite07),
	Version::lite(lite::Version::Lite06),
	Version::lite(lite::Version::Lite05),
	Version::lite(lite::Version::Lite04),
	Version::lite(lite::Version::Lite03),
	Version::lite(lite::Version::Lite02),
	Version::lite(lite::Version::Lite01),
	Version::ietf(ietf::Version::Draft23),
	Version::ietf(ietf::Version::Draft22),
	Version::ietf(ietf::Version::Draft20),
	Version::ietf(ietf::Version::Draft18),
];

/// The draft a [`Version`] names, which only this crate tells apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Draft {
	/// A `moq-lite` draft, the simplified protocol this project specifies.
	Lite(lite::Version),
	/// An IETF `moq-transport` draft.
	Ietf(ietf::Version),
}

/// A MoQ protocol version, such as `moq-lite-06`.
///
/// Opaque: parse it ([`FromStr`]), print it, and ask [`is_lite`](Self::is_lite) or
/// [`alpn`](Self::alpn), so adding or dropping a draft is never an API change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version(pub(crate) Draft);

impl Version {
	pub(crate) const fn lite(version: lite::Version) -> Self {
		Self(Draft::Lite(version))
	}

	pub(crate) const fn ietf(version: ietf::Version) -> Self {
		Self(Draft::Ietf(version))
	}

	/// Every version this crate speaks, newest first, including the opt-in `-wip` drafts
	/// that [`Versions::all`] leaves out.
	pub const ALL: &'static [Version] = &ALL;

	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn session_code(self, error: &crate::SessionError) -> u32 {
		match self.0 {
			Draft::Lite(_) => crate::lite::error::to_session_code(error),
			Draft::Ietf(_) => crate::ietf::error::to_session_code(error),
		}
	}

	/// Whether the client's SETUP can carry its connection credential (see
	/// `Client::with_token`, behind the `async` feature). Every moq-transport draft can;
	/// moq-lite gained it in lite-07, so older versions need the credential in the URL.
	pub(crate) fn has_setup_token(self) -> bool {
		match self.0 {
			Draft::Lite(version) => version.has_auth(),
			Draft::Ietf(_) => true,
		}
	}

	/// Whether SETUP carries the request path, so a transport with no request URI (native
	/// QUIC, qmux over TCP) can still name its target: moq-lite-05 and newer, and every
	/// moq-transport draft.
	pub fn has_request_path(&self) -> bool {
		match self.0 {
			Draft::Lite(version) => version.has_setup_stream(),
			Draft::Ietf(_) => true,
		}
	}

	/// Handshake / config name, e.g. `moq-lite-05` or `moq-transport-22`.
	pub fn as_str(&self) -> &'static str {
		match self.0 {
			Draft::Lite(lite::Version::Lite01) => "moq-lite-01",
			Draft::Lite(lite::Version::Lite02) => "moq-lite-02",
			Draft::Lite(lite::Version::Lite03) => "moq-lite-03",
			Draft::Lite(lite::Version::Lite04) => "moq-lite-04",
			Draft::Lite(lite::Version::Lite05) => "moq-lite-05",
			Draft::Lite(lite::Version::Lite06) => "moq-lite-06",
			Draft::Lite(lite::Version::Lite07) => "moq-lite-07-wip",
			Draft::Ietf(ietf::Version::Draft18) => "moq-transport-18",
			Draft::Ietf(ietf::Version::Draft20) => "moq-transport-20",
			Draft::Ietf(ietf::Version::Draft22) => "moq-transport-22",
			Draft::Ietf(ietf::Version::Draft23) => "moq-transport-23",
		}
	}

	/// Parse from wire version code (used during SETUP negotiation).
	pub(crate) fn from_code(code: u64) -> Option<Self> {
		match code {
			0xff0dad01 => Some(Self::lite(lite::Version::Lite01)),
			0xff0dad02 => Some(Self::lite(lite::Version::Lite02)),
			0xff0dad03 => Some(Self::lite(lite::Version::Lite03)),
			0xff0dad04 => Some(Self::lite(lite::Version::Lite04)),
			0xff0dad05 => Some(Self::lite(lite::Version::Lite05)),
			0xff0dad06 => Some(Self::lite(lite::Version::Lite06)),
			0xff0dad07 => Some(Self::lite(lite::Version::Lite07)),
			0xff000012 => Some(Self::ietf(ietf::Version::Draft18)),
			0xff000014 => Some(Self::ietf(ietf::Version::Draft20)),
			0xff000016 => Some(Self::ietf(ietf::Version::Draft22)),
			0xff000017 => Some(Self::ietf(ietf::Version::Draft23)),
			_ => None,
		}
	}

	/// Get the wire version code.
	pub(crate) fn code(&self) -> u64 {
		match self.0 {
			Draft::Lite(lite::Version::Lite01) => 0xff0dad01,
			Draft::Lite(lite::Version::Lite02) => 0xff0dad02,
			Draft::Lite(lite::Version::Lite03) => 0xff0dad03,
			Draft::Lite(lite::Version::Lite04) => 0xff0dad04,
			Draft::Lite(lite::Version::Lite05) => 0xff0dad05,
			Draft::Lite(lite::Version::Lite06) => 0xff0dad06,
			Draft::Lite(lite::Version::Lite07) => 0xff0dad07,
			Draft::Ietf(ietf::Version::Draft18) => 0xff000012,
			Draft::Ietf(ietf::Version::Draft20) => 0xff000014,
			Draft::Ietf(ietf::Version::Draft22) => 0xff000016,
			Draft::Ietf(ietf::Version::Draft23) => 0xff000017,
		}
	}

	/// Returns the ALPN string for this version.
	pub fn alpn(&self) -> &'static str {
		match self.0 {
			Draft::Lite(lite::Version::Lite07) => ALPN_LITE_07_WIP,
			Draft::Lite(lite::Version::Lite06) => ALPN_LITE_06,
			Draft::Lite(lite::Version::Lite05) => ALPN_LITE_05,
			Draft::Lite(lite::Version::Lite04) => ALPN_LITE_04,
			Draft::Lite(lite::Version::Lite03) => ALPN_LITE_03,
			Draft::Lite(lite::Version::Lite01 | lite::Version::Lite02) => ALPN_LITE,
			Draft::Ietf(ietf::Version::Draft18) => ALPN_18,
			Draft::Ietf(ietf::Version::Draft20) => ALPN_20,
			Draft::Ietf(ietf::Version::Draft22) => ALPN_22,
			Draft::Ietf(ietf::Version::Draft23) => ALPN_23,
		}
	}

	/// Whether SETUP selecting this version ends the SETUP stream, the session continuing
	/// as this version's ALPN would rather than on that stream.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn hands_off(self) -> bool {
		matches!(self.0, Draft::Lite(v) if !matches!(v, lite::Version::Lite01 | lite::Version::Lite02))
	}

	/// Whether this is a lite protocol version.
	pub fn is_lite(&self) -> bool {
		match self.0 {
			Draft::Lite(_) => true,
			Draft::Ietf(_) => false,
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
	/// `Draft23` is excluded the same way while its port is incomplete.
	pub fn all() -> Self {
		Self(
			ALL.iter()
				.filter(|version| {
					!matches!(
						version.0,
						Draft::Lite(lite::Version::Lite07) | Draft::Ietf(ietf::Version::Draft23)
					)
				})
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
		let newest = Version::lite(lite::Version::Lite06);
		assert_eq!(newest.alpn(), "moq-lite-06");
		assert!("moq-lite-06-wip".parse::<Version>().is_err());
		assert_eq!(Versions::all().iter().next(), Some(&newest));
		assert_eq!(Versions::all().alpns().first(), Some(&newest.alpn()));
		assert_eq!(ALPNS.first(), Some(&newest.alpn()));
	}

	#[test]
	fn lite_07_wip_is_opt_in_only() {
		let wip = Version::lite(lite::Version::Lite07);
		assert_eq!(wip.alpn(), "moq-lite-07-wip");
		assert_eq!(wip.to_string(), "moq-lite-07-wip");
		assert_eq!("moq-lite-07-wip".parse::<Version>(), Ok(wip));
		assert!("moq-lite-07".parse::<Version>().is_err());

		assert!(!Versions::all().contains(&wip));
		assert!(!ALPNS.contains(&wip.alpn()));
		assert_eq!(Versions::from(wip).alpns(), [wip.alpn()]);
	}
}
