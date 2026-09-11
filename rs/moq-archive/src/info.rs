use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::path::check_id;
use crate::{Error, Result, VERSION};

/// Immutable track properties stored at `<encoded-track>/.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Info {
	/// Recording format version; must be [`VERSION`].
	pub version: u64,
	/// Publisher tie-break priority, 0 through 255.
	pub priority: u8,
	/// Units per second for this track's timestamps, 1 through 2^53 - 1.
	pub timescale: u64,
}

impl Info {
	/// Format version 1 with `priority` and `timescale`.
	pub fn new(priority: u8, timescale: u64) -> Result<Self> {
		let info = Self {
			version: VERSION,
			priority,
			timescale,
		};
		info.validate()?;
		Ok(info)
	}

	/// Encode as compact JSON.
	pub fn encode(&self) -> Result<Bytes> {
		self.validate()?;
		Ok(Bytes::from(serde_json::to_vec(self)?))
	}

	/// Decode JSON and refuse an unknown version or invalid timescale.
	pub fn decode(bytes: &[u8]) -> Result<Self> {
		let info: Self = serde_json::from_slice(bytes)?;
		info.validate()?;
		Ok(info)
	}

	fn validate(&self) -> Result<()> {
		if self.version != VERSION {
			return Err(Error::Version(self.version));
		}
		if self.timescale == 0 {
			return Err(Error::Timescale(self.timescale));
		}
		check_id(self.timescale).map_err(|_| Error::Timescale(self.timescale))?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ID_MAX;

	#[test]
	fn roundtrip() {
		let info = Info::new(0, 1_000_000).unwrap();
		let bytes = info.encode().unwrap();
		assert_eq!(bytes.as_ref(), br#"{"version":1,"priority":0,"timescale":1000000}"#);
		assert_eq!(Info::decode(&bytes).unwrap(), info);
	}

	#[test]
	fn whitespace_and_member_order_do_not_affect_equality() {
		let info = Info::decode(br#"{"timescale":1000,"priority":7,"version":1}"#).unwrap();
		assert_eq!(
			info,
			Info::decode(br#"{ "version": 1, "priority": 7, "timescale": 1000 }"#).unwrap()
		);
		assert_eq!(info.priority, 7);
		assert_eq!(info.timescale, 1000);
	}

	#[test]
	fn unknown_version_is_refused() {
		assert!(matches!(
			Info::decode(br#"{"version":2,"priority":0,"timescale":1000}"#),
			Err(Error::Version(2))
		));
	}

	#[test]
	fn timescale_bounds() {
		assert!(Info::new(0, 1).is_ok());
		assert!(Info::new(0, ID_MAX).is_ok());
		assert!(matches!(Info::new(0, 0), Err(Error::Timescale(0))));
		assert!(matches!(Info::new(0, ID_MAX + 1), Err(Error::Timescale(_))));
	}

	#[test]
	fn unknown_members_and_missing_fields_are_refused() {
		assert!(matches!(
			Info::decode(br#"{"version":1,"priority":0,"timescale":1,"extra":true}"#),
			Err(Error::Json(_))
		));
		assert!(matches!(
			Info::decode(br#"{"version":1,"priority":0}"#),
			Err(Error::Json(_))
		));
	}
}
