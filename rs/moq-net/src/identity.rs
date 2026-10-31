use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// Names one continuous stream at a path.
///
/// Within an identity, group sequences never restart and never name two different groups,
/// so a reader continues across every route that shares it. Different identities are
/// independent streams, never stitched together.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Identity(Arc<str>);

/// What every [minted](Identity::is_minted) identity starts with.
const MINTED: char = '~';

/// The identity is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid identity: empty")]
#[non_exhaustive]
pub struct InvalidIdentity;

impl Identity {
	/// A fresh random identity, for a new stream.
	pub fn random() -> Self {
		Self(random().into())
	}

	/// A fresh identity a session mints for a route its wire delivered without one.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn minted() -> Self {
		Self(format!("{MINTED}{}", random()).into())
	}

	/// The identity's text.
	pub fn as_str(&self) -> &str {
		&self.0
	}

	/// Whether a relay minted this identity, for a route whose wire (an older moq-lite, or
	/// moq-transport) carries none: its text starts with `~`. It names the announcement
	/// rather than the publisher's stream, so it can neither match nor contradict an identity
	/// the publisher named elsewhere, such as in a catalog. Hops that forward it keep the mark.
	pub fn is_minted(&self) -> bool {
		self.0.starts_with(MINTED)
	}
}

/// 128 random bits as hex.
fn random() -> String {
	use rand::RngExt;
	format!("{:032x}", rand::rng().random::<u128>())
}

impl FromStr for Identity {
	type Err = InvalidIdentity;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		match text.is_empty() {
			true => Err(InvalidIdentity),
			false => Ok(Self(text.into())),
		}
	}
}

impl TryFrom<String> for Identity {
	type Error = InvalidIdentity;

	fn try_from(text: String) -> Result<Self, Self::Error> {
		match text.is_empty() {
			true => Err(InvalidIdentity),
			false => Ok(Self(text.into())),
		}
	}
}

impl fmt::Debug for Identity {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("Identity").field(&self.as_str()).finish()
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
		let minted = Identity::minted();
		assert!(minted.is_minted());
		assert_eq!(
			minted.as_str().parse::<Identity>().unwrap(),
			minted,
			"a forwarded mark survives"
		);
	}
}
