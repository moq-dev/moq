use std::fmt;
use std::str::FromStr;
use std::sync::Arc;

use crate::error::{Error, Result};
use crate::limits::check_bytes;

/// One publisher instance's scope: a nonempty path segment that no other instance under the credential reuses.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Epoch(Arc<str>);

impl Epoch {
	/// Mint a fresh epoch: the lowercase text of a UUID version 7, which sorts newest last.
	pub fn mint() -> Self {
		Self(uuid::Uuid::now_v7().hyphenated().to_string().into())
	}

	/// The epoch text, the last segment of the protected broadcast path.
	pub fn as_str(&self) -> &str {
		&self.0
	}
}

impl FromStr for Epoch {
	type Err = Error;

	/// Accept an epoch discovered from a path: nonempty, no `/`, at most 65535 bytes.
	fn from_str(text: &str) -> Result<Self> {
		if text.is_empty() || text.contains('/') {
			return Err(Error::Identity);
		}
		check_bytes(text.len())?;
		Ok(Self(text.into()))
	}
}

impl fmt::Debug for Epoch {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		f.debug_tuple("Epoch").field(&self.as_str()).finish()
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
