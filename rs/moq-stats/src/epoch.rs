use std::{fmt, str::FromStr, sync::Arc};

/// Names one run of a bucket's counters, independently of its broadcast's instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde_with::SerializeDisplay, serde_with::DeserializeFromStr)]
pub struct Epoch(Arc<str>);

/// The bucket epoch is empty or holds a `/`: it names a bucket track's last
/// segment, so it must round-trip through one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid bucket epoch: empty or contains '/'")]
#[non_exhaustive]
pub struct InvalidEpoch;

impl Epoch {
	/// Mint a UUIDv7 so locally produced epochs sort by creation.
	pub(crate) fn mint() -> Self {
		Self(uuid::Uuid::now_v7().hyphenated().to_string().into())
	}

	/// The epoch's text.
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl FromStr for Epoch {
	type Err = InvalidEpoch;

	fn from_str(text: &str) -> Result<Self, Self::Err> {
		if text.is_empty() || text.contains('/') {
			return Err(InvalidEpoch);
		}
		Ok(Self(text.into()))
	}
}

impl TryFrom<String> for Epoch {
	type Error = InvalidEpoch;

	fn try_from(text: String) -> Result<Self, Self::Error> {
		if text.is_empty() || text.contains('/') {
			return Err(InvalidEpoch);
		}
		Ok(Self(text.into()))
	}
}

impl fmt::Display for Epoch {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.write_str(self.as_str())
	}
}

impl AsRef<str> for Epoch {
	fn as_ref(&self) -> &str {
		self.as_str()
	}
}
