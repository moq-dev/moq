use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::path::check_id;
use crate::{Error, Result, VERSION, VERSION_MIN};

/// Immutable track properties stored at `<encoded-track>/.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Stored")]
pub struct Info {
	/// Recording format version: [`VERSION`], or 2 for a recording read from before
	/// priorities were signed, which only a reader accepts.
	pub version: u64,
	/// The publisher's track priority, higher first.
	pub priority: i8,
	/// Units per second for this track's timestamps, 1 through 2^53 - 1, or `None` when the track
	/// is untimed and its frames store no timestamps.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub timescale: Option<u64>,
}

/// [`Info`] as stored, before its priority is read in its version's encoding.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
	version: u64,
	priority: i64,
	#[serde(default)]
	timescale: Option<u64>,
}

impl TryFrom<Stored> for Info {
	type Error = String;

	fn try_from(stored: Stored) -> std::result::Result<Self, Self::Error> {
		let priority = match stored.version {
			// Version 2 stored the moq-lite byte, `priority + 128`.
			2 => u8::try_from(stored.priority).map(|byte| (byte ^ 0x80) as i8),
			_ => i8::try_from(stored.priority),
		}
		.map_err(|_| format!("priority {} is out of range", stored.priority))?;
		Ok(Self {
			version: stored.version,
			priority,
			timescale: stored.timescale,
		})
	}
}

impl Info {
	/// The current format version with `priority` and `timescale`, `None` for an untimed track.
	pub fn new(priority: i8, timescale: Option<u64>) -> Result<Self> {
		let info = Self {
			version: VERSION,
			priority,
			timescale,
		};
		info.validate()?;
		Ok(info)
	}

	/// Encode as compact JSON, refusing any version but the current one.
	pub fn encode(&self) -> Result<Bytes> {
		if self.version != VERSION {
			return Err(Error::Version(self.version));
		}
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
		if !(VERSION_MIN..=VERSION).contains(&self.version) {
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
		assert_eq!(bytes.as_ref(), br#"{"version":3,"priority":0,"timescale":1000000}"#);
		assert_eq!(Info::decode(&bytes).unwrap(), info);
	}

	#[test]
	fn an_untimed_track_omits_its_timescale() {
		let info = Info::new(3, None).unwrap();
		let bytes = info.encode().unwrap();
		assert_eq!(bytes.as_ref(), br#"{"version":3,"priority":3}"#);
		assert_eq!(Info::decode(&bytes).unwrap(), info);
	}

	#[test]
	fn whitespace_and_member_order_do_not_affect_equality() {
		let info = Info::decode(br#"{"timescale":1000,"priority":7,"version":3}"#).unwrap();
		assert_eq!(
			info,
			Info::decode(br#"{ "version": 3, "priority": 7, "timescale": 1000 }"#).unwrap()
		);
		assert_eq!(info.priority, 7);
		assert_eq!(info.timescale, Some(1000));
	}

	#[test]
	fn priority_is_stored_signed() {
		for priority in [i8::MIN, -1, 0, i8::MAX] {
			let info = Info::new(priority, None).unwrap();
			let json = format!(r#"{{"version":3,"priority":{priority}}}"#);
			assert_eq!(info.encode().unwrap().as_ref(), json.as_bytes());
			assert_eq!(Info::decode(json.as_bytes()).unwrap(), info);
		}
		for priority in [-129, 128] {
			let json = format!(r#"{{"version":3,"priority":{priority}}}"#);
			assert!(
				matches!(Info::decode(json.as_bytes()), Err(Error::Json(_))),
				"{priority}"
			);
		}
	}

	/// Version 2 stored the priority as its moq-lite byte, `priority + 128`, so an old
	/// recording keeps its meaning; nothing writes version 2 any more.
	#[test]
	fn a_version_2_info_reads_its_lite_byte() {
		for (byte, priority) in [(0, i8::MIN), (127, -1), (128, 0), (255, i8::MAX)] {
			let json = format!(r#"{{"version":2,"priority":{byte},"timescale":1000}}"#);
			let info = Info::decode(json.as_bytes()).unwrap();
			assert_eq!((info.version, info.priority, info.timescale), (2, priority, Some(1000)));
			assert!(matches!(info.encode(), Err(Error::Version(2))));
		}
		for byte in [-1, 256] {
			let json = format!(r#"{{"version":2,"priority":{byte}}}"#);
			assert!(matches!(Info::decode(json.as_bytes()), Err(Error::Json(_))), "{byte}");
		}
	}

	#[test]
	fn unknown_version_is_refused() {
		for version in [1, VERSION + 1] {
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
	fn unknown_members_and_missing_fields_are_refused() {
		assert!(matches!(
			Info::decode(br#"{"version":2,"priority":128,"timescale":1000,"epoch":"x"}"#),
			Err(Error::Json(_))
		));
		assert!(matches!(
			Info::decode(br#"{"version":3,"priority":0,"timescale":1,"extra":true}"#),
			Err(Error::Json(_))
		));
		assert!(matches!(
			Info::decode(br#"{"version":3,"timescale":1}"#),
			Err(Error::Json(_))
		));
	}
}
