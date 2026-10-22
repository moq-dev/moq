use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::path::check_id;
use crate::{Error, Result, VERSION};

/// Immutable track properties stored at `<encoded-track>/.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "Stored")]
pub struct Info {
	/// Recording format version; must be [`VERSION`].
	pub version: u64,
	/// Publisher tie-break priority, 0 through 255.
	pub priority: u8,
	/// Units per second for this track's timestamps, 1 through 2^53 - 1, or `None` when the track
	/// is untimed and its frames store no timestamps.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timescale: Option<u64>,
}

/// [`Info`] as stored, also taking the `epoch` an older `Writer` recorded, so [`Reader`](crate::Reader)
/// still replays those recordings; it goes when `Reader` does.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
	version: u64,
	priority: u8,
	#[serde(default)]
	timescale: Option<u64>,
	#[serde(default, rename = "epoch")]
	_epoch: Option<serde::de::IgnoredAny>,
}

impl From<Stored> for Info {
	fn from(stored: Stored) -> Self {
		Self {
			version: stored.version,
			priority: stored.priority,
			timescale: stored.timescale,
		}
	}
}

impl Info {
	/// The current format version with `priority` and `timescale`, `None` for an untimed track.
	pub fn new(priority: u8, timescale: Option<u64>) -> Result<Self> {
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
		if let Some(timescale) = self.timescale {
			if timescale == 0 {
				return Err(Error::Timescale(timescale));
			}
			check_id(timescale).map_err(|_| Error::Timescale(timescale))?;
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::ID_MAX;

	#[test]
	fn roundtrip() {
		let info = Info::new(0, Some(1_000_000)).unwrap();
		let bytes = info.encode().unwrap();
		assert_eq!(bytes.as_ref(), br#"{"version":2,"priority":0,"timescale":1000000}"#);
		assert_eq!(Info::decode(&bytes).unwrap(), info);
	}

	#[test]
	fn an_untimed_track_omits_its_timescale() {
		let info = Info::new(3, None).unwrap();
		let bytes = info.encode().unwrap();
		assert_eq!(bytes.as_ref(), br#"{"version":2,"priority":3}"#);
		assert_eq!(Info::decode(&bytes).unwrap(), info);
	}

	#[test]
	fn whitespace_and_member_order_do_not_affect_equality() {
		let info = Info::decode(br#"{"timescale":1000,"priority":7,"version":2}"#).unwrap();
		assert_eq!(
			info,
			Info::decode(br#"{ "version": 2, "priority": 7, "timescale": 1000 }"#).unwrap()
		);
		assert_eq!(info.priority, 7);
		assert_eq!(info.timescale, Some(1000));
	}

	#[test]
	fn unknown_version_is_refused() {
		for version in [1, 3] {
			let json = format!(r#"{{"version":{version},"priority":0,"timescale":1000}}"#);
			assert!(matches!(Info::decode(json.as_bytes()), Err(Error::Version(v)) if v == version));
		}
	}

	#[test]
	fn timescale_bounds() {
		assert!(Info::new(0, Some(1)).is_ok());
		assert!(Info::new(0, Some(ID_MAX)).is_ok());
		assert!(matches!(Info::new(0, Some(0)), Err(Error::Timescale(0))));
		assert!(matches!(Info::new(0, Some(ID_MAX + 1)), Err(Error::Timescale(_))));
	}

	#[test]
	fn an_older_writers_epoch_is_ignored() {
		let info = Info::decode(br#"{"version":2,"priority":0,"timescale":1000,"epoch":"x"}"#).unwrap();
		assert_eq!(info, Info::new(0, Some(1000)).unwrap());
		assert_eq!(
			info.encode().unwrap().as_ref(),
			br#"{"version":2,"priority":0,"timescale":1000}"#
		);
	}

	#[test]
	fn unknown_members_and_missing_fields_are_refused() {
		assert!(matches!(
			Info::decode(br#"{"version":2,"priority":0,"timescale":1,"extra":true}"#),
			Err(Error::Json(_))
		));
		assert!(matches!(
			Info::decode(br#"{"version":2,"timescale":1}"#),
			Err(Error::Json(_))
		));
	}
}
