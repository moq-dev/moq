use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::path::check_id;
use crate::{Error, Result, VERSION};

/// Immutable track properties stored at `<encoded-track>/.info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
	/// The publisher's track priority, higher first.
	pub priority: i8,
	/// Units per second for this track's timestamps, 1 through 2^53 - 1, or `None` when the track
	/// is untimed and its frames store no timestamps.
	pub timescale: Option<u64>,
}

/// [`Info`] as stored, with the format version. Unknown members are ignored, so a later writer
/// can add one a reader may skip.
#[derive(Serialize, Deserialize)]
struct Stored {
	version: u64,
	priority: i8,
	#[serde(default, skip_serializing_if = "Option::is_none")]
	timescale: Option<u64>,
}

impl Info {
	/// The properties `priority` and `timescale`, `None` for an untimed track.
	pub fn new(priority: i8, timescale: Option<u64>) -> Result<Self> {
		let info = Self { priority, timescale };
		info.validate()?;
		Ok(info)
	}

	/// Encode as compact JSON in the current format version.
	pub fn encode(&self) -> Result<Bytes> {
		self.validate()?;
		let stored = Stored {
			version: VERSION,
			priority: self.priority,
			timescale: self.timescale,
		};
		Ok(Bytes::from(serde_json::to_vec(&stored)?))
	}

	/// Decode JSON, refusing any version but [`VERSION`] or an invalid timescale.
	pub fn decode(bytes: &[u8]) -> Result<Self> {
		// Read the version alone first, so another version's members are never misread as ours.
		#[derive(Deserialize)]
		struct Version {
			version: u64,
		}
		let Version { version } = serde_json::from_slice(bytes)?;
		if version != VERSION {
			return Err(Error::Version(version));
		}
		let stored: Stored = serde_json::from_slice(bytes)?;
		let info = Self {
			priority: stored.priority,
			timescale: stored.timescale,
		};
		info.validate()?;
		Ok(info)
	}

	fn validate(&self) -> Result<()> {
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
		assert_eq!(info, Info::new(7, Some(1000)).unwrap());
	}

	#[test]
	fn priority_is_stored_signed() {
		for priority in [i8::MIN, -1, 0, i8::MAX] {
			let info = Info::new(priority, None).unwrap();
			let json = format!(r#"{{"version":2,"priority":{priority}}}"#);
			assert_eq!(info.encode().unwrap().as_ref(), json.as_bytes());
			assert_eq!(Info::decode(json.as_bytes()).unwrap(), info);
		}
		for priority in [-129, 128] {
			let json = format!(r#"{{"version":2,"priority":{priority}}}"#);
			assert!(
				matches!(Info::decode(json.as_bytes()), Err(Error::Json(_))),
				"{priority}"
			);
		}
	}

	/// Release wrote version 1, and main builds before the format shipped wrote 3 and 4, each
	/// meaning something else: every one is refused by its version, whatever its other members.
	#[test]
	fn another_version_is_refused_by_its_number() {
		for json in [
			r#"{"version":1,"priority":255,"timescale":1000}"#,
			r#"{"version":3,"priority":-1,"timescale":1000}"#,
			r#"{"version":4,"priority":0}"#,
			r#"{"version":3,"epoch":"x"}"#,
		] {
			let Err(Error::Version(version)) = Info::decode(json.as_bytes()) else {
				panic!("{json} decoded");
			};
			assert_ne!(version, VERSION);
		}
		assert_eq!(
			Error::Version(4).to_string(),
			"archive format version 4 is not supported, only version 2"
		);
	}

	#[test]
	fn timescale_bounds() {
		assert!(Info::new(0, Some(1)).is_ok());
		assert!(Info::new(0, Some(ID_MAX)).is_ok());
		assert!(matches!(Info::new(0, Some(0)), Err(Error::Timescale(0))));
		assert!(matches!(Info::new(0, Some(ID_MAX + 1)), Err(Error::Timescale(_))));
		assert!(matches!(
			Info::decode(br#"{"version":2,"priority":0,"timescale":0}"#),
			Err(Error::Timescale(0))
		));
	}

	/// A member a later writer adds is skipped, so the format can grow without a new version.
	#[test]
	fn unknown_members_are_ignored() {
		let info = Info::decode(br#"{"version":2,"priority":1,"timescale":90000,"label":{"en":"x"}}"#).unwrap();
		assert_eq!(info, Info::new(1, Some(90000)).unwrap());
	}

	#[test]
	fn missing_members_are_refused() {
		for json in [r#"{"version":2,"timescale":1}"#, r#"{"priority":0}"#, "[]"] {
			assert!(matches!(Info::decode(json.as_bytes()), Err(Error::Json(_))), "{json}");
		}
	}
}
