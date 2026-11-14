use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

/// Names one continuous stream at a path.
///
/// Within an instance, group keys never restart and never name two different groups,
/// so a reader continues across every route that shares it. Different instances are
/// independent streams, never stitched together.
///
/// Two instances are equal only when both their text and whether they were
/// [minted](Instance::is_minted) match, so a named instance never matches a minted one.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Instance {
	text: Arc<str>,
	minted: bool,
}

/// The instance is empty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid instance: empty")]
#[non_exhaustive]
pub struct InvalidInstance;

impl Instance {
	/// A fresh random instance, for a new stream.
	pub fn random() -> Self {
		Self::named(random().into())
	}

	fn named(text: Arc<str>) -> Self {
		Self { text, minted: false }
	}

	/// A fresh instance a session mints for a route its wire delivered without one.
	#[cfg_attr(not(feature = "async"), allow(dead_code, reason = "async-only"))]
	pub(crate) fn minted() -> Self {
		Self {
			text: random().into(),
			minted: true,
		}
	}

	/// An instance a lite-07 peer carried, keeping whether a session minted it.
	pub(crate) fn decoded(text: &str, minted: bool) -> Result<Self, InvalidInstance> {
		Ok(Self {
			minted,
			..text.parse()?
		})
	}

	/// The instance's text, which alone does not tell a minted instance from a named one.
	pub fn as_str(&self) -> &str {
		&self.text
	}

	/// Whether a session minted this instance, for a route whose wire (an older moq-lite, or
	/// moq-transport) carries none. It names the announcement rather than the publisher's
	/// stream, so it can neither match nor contradict an instance the publisher named
	/// elsewhere, such as in a catalog. Hops that forward it keep the flag, and only a
	/// session sets it: parsing text never builds a minted instance.
	pub fn is_minted(&self) -> bool {
		self.minted
	}
}

/// 128 random bits as hex, from the ambient generator so a simulated run replays them.
fn random() -> String {
	use rand::RngExt;
	format!("{:032x}", kio::clock::rng().random::<u128>())
}

impl FromStr for Instance {
	type Err = InvalidInstance;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		match text.is_empty() {
			true => Err(InvalidInstance),
			false => Ok(Self::named(text.into())),
		}
	}
}

impl TryFrom<String> for Instance {
	type Error = InvalidInstance;

	fn try_from(text: String) -> Result<Self, Self::Error> {
		match text.is_empty() {
			true => Err(InvalidInstance),
			false => Ok(Self::named(text.into())),
		}
	}
}

impl fmt::Debug for Instance {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		let mut debug = f.debug_tuple("Instance");
		debug.field(&self.as_str());
		if self.minted {
			debug.field(&"minted");
		}
		debug.finish()
	}
}

impl fmt::Display for Instance {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl AsRef<str> for Instance {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn random_is_fresh_and_round_trips() {
		let instance = Instance::random();
		assert_ne!(instance, Instance::random());
		assert_eq!(instance.as_str().parse::<Instance>().unwrap(), instance);
		assert!("".parse::<Instance>().is_err());
		assert!(!instance.is_minted());
	}

	#[test]
	fn text_never_builds_a_minted_instance() {
		let minted = Instance::minted();
		assert!(minted.is_minted());
		let named: Instance = minted.as_str().parse().unwrap();
		assert!(!named.is_minted());
		assert_ne!(named, minted, "the same text, named, is another instance");
		assert!(!"~tilde".parse::<Instance>().unwrap().is_minted());
		assert_eq!(Instance::decoded(minted.as_str(), true).unwrap(), minted);
		assert!(Instance::decoded("", true).is_err());
	}
}
