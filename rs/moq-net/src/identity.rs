use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// Names one continuous stream at a path.
///
/// Within an identity, group keys never restart and never name two different groups,
/// so a reader continues across every route that shares it. Different identities are
/// independent streams, never stitched together.
///
/// Two identities are equal only when both their text and whether they were
/// [minted](Identity::is_minted) match, so a named identity never matches a minted one.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identity {
	text: Arc<str>,
	minted: bool,
}

/// The identity is empty or longer than 256 UTF-8 bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid identity: {0}")]
#[non_exhaustive]
pub struct InvalidIdentity(&'static str);

/// The most UTF-8 bytes an identity holds: room for a public key in any common text encoding
/// (hex, base64url, `did:key`, or a JWK) while bounding what a claim makes a relay hold.
pub(crate) const MAX_LEN: usize = 256;

impl Identity {
	/// A fresh random identity, for a new stream.
	pub fn random() -> Self {
		Self::named(random().into())
	}

	fn named(text: Arc<str>) -> Self {
		Self { text, minted: false }
	}

	/// A fresh identity a session mints for a route its wire delivered without one.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn minted() -> Self {
		Self {
			text: random().into(),
			minted: true,
		}
	}

	/// An identity a lite-07 peer carried, keeping whether a session minted it.
	pub(crate) fn decoded(text: &str, minted: bool) -> Result<Self, InvalidIdentity> {
		Ok(Self {
			minted,
			..text.parse()?
		})
	}

	/// The identity's text, which alone does not tell a minted identity from a named one.
	pub fn as_str(&self) -> &str {
		&self.text
	}

	/// Whether a session minted this identity, for a route whose wire (an older moq-lite, or
	/// moq-transport) carries none. It names the announcement rather than the publisher's
	/// stream, so it can neither match nor contradict an identity the publisher named
	/// elsewhere, such as in a catalog. Hops that forward it keep the flag, and only a
	/// session sets it: parsing text never builds a minted identity.
	pub fn is_minted(&self) -> bool {
		self.minted
	}
}

/// 128 random bits as hex, from the ambient generator so a simulated run replays them.
fn random() -> String {
	use rand::RngExt;
	format!("{:032x}", kio::clock::rng().random::<u128>())
}

impl FromStr for Identity {
	type Err = InvalidIdentity;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		check(text)?;
		Ok(Self::named(text.into()))
	}
}

/// Refuse an empty identity, or one longer than [`MAX_LEN`] bytes.
fn check(text: &str) -> Result<(), InvalidIdentity> {
	match text.len() {
		0 => Err(InvalidIdentity("empty")),
		len if len > MAX_LEN => Err(InvalidIdentity("longer than 256 bytes")),
		_ => Ok(()),
	}
}

impl TryFrom<String> for Identity {
	type Error = InvalidIdentity;

	fn try_from(text: String) -> Result<Self, Self::Error> {
		check(&text)?;
		Ok(Self::named(text.into()))
	}
}

impl fmt::Debug for Identity {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let mut debug = f.debug_tuple("Identity");
		debug.field(&self.as_str());
		if self.minted {
			debug.field(&"minted");
		}
		debug.finish()
	}
}

impl fmt::Display for Identity {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl AsRef<str> for Identity {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn random_is_fresh_and_round_trips() {
		let identity = Identity::random();
		assert_ne!(identity, Identity::random());
		assert_eq!(identity.as_str().parse::<Identity>().unwrap(), identity);
		assert!("".parse::<Identity>().is_err());
		assert!(!identity.is_minted());
	}

	#[test]
	fn text_never_builds_a_minted_identity() {
		let minted = Identity::minted();
		assert!(minted.is_minted());
		let named: Identity = minted.as_str().parse().unwrap();
		assert!(!named.is_minted());
		assert_ne!(named, minted, "the same text, named, is another identity");
		assert!(!"~tilde".parse::<Identity>().unwrap().is_minted());
		assert_eq!(Identity::decoded(minted.as_str(), true).unwrap(), minted);
		assert!(Identity::decoded("", true).is_err());
	}

	#[test]
	fn caps_at_256_utf8_bytes() {
		let full = "é".repeat(128);
		assert_eq!(full.parse::<Identity>().unwrap().as_str(), full);
		assert!(format!("{full}a").parse::<Identity>().is_err());
		assert!(Identity::try_from(format!("{full}a")).is_err());
		assert!(Identity::decoded(&format!("{full}a"), true).is_err());
	}
}
