use std::{borrow::Cow, time::Duration};

use crate::{
	Path, Timescale,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

// Older JS readers use safe integer milliseconds; larger legacy ages mean no limit.
const LEGACY_UNLIMITED: u64 = (1u64 << 53) - 1;

/// Sent by the subscriber on a Track Stream (0x6) to request a track's immutable
/// publisher properties, without subscribing or fetching.
///
/// Lite05+ only.
#[derive(Clone, Debug)]
pub struct Track<'a> {
	pub broadcast: Path<'a>,
	pub track: Cow<'a, str>,
}

impl Message for Track<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}

		let broadcast = Path::decode(r, version)?;
		let track = Cow::<str>::decode(r, version)?;

		Ok(Self { broadcast, track })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}

		self.broadcast.encode(w, version)?;
		self.track.encode(w, version)?;
		Ok(())
	}
}

/// The publisher's sole reply on a Track Stream, carrying the track's immutable
/// properties. Every field is fixed for the lifetime of the track, so a subscriber
/// fetches this once and reuses it across every SUBSCRIBE and FETCH.
///
/// Lite05+ only.
#[derive(Clone, Debug)]
pub struct TrackInfo {
	/// The publisher's tie-break priority for this track.
	pub priority: u8,
	/// Publisher Max Age: an upper bound on how long the publisher caches a
	/// non-latest group past the arrival of a newer one. Encoded as milliseconds.
	pub max_age: Option<Duration>,
	/// Per-frame timestamp scale (units per second). Mandatory on Lite05+: every track
	/// is timed, so this is always a real scale on the wire (never zero).
	pub timescale: Timescale,
}

impl Message for TrackInfo {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}

		let priority = u8::decode(r, version)?;
		super::subscribe::skip_group_order(r, version)?;
		let encoded = u64::decode(r, version)?;
		let max_age = match version {
			Version::Lite05 | Version::Lite06 => (encoded < LEGACY_UNLIMITED).then(|| Duration::from_millis(encoded)),
			_ => encoded.checked_sub(1).map(Duration::from_millis),
		};
		let timescale = Timescale::new(u64::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)?;

		Ok(Self {
			priority,
			max_age,
			timescale,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}

		self.priority.encode(w, version)?;
		super::subscribe::pad_group_order(w, version)?;
		let encoded = match (version, self.max_age) {
			(Version::Lite05 | Version::Lite06, None) => LEGACY_UNLIMITED,
			(Version::Lite05 | Version::Lite06, Some(age)) => age.as_millis().min(u128::from(LEGACY_UNLIMITED)) as u64,
			(_, None) => 0,
			(_, Some(age)) => u64::try_from(age.as_millis() + 1).map_err(|_| EncodeError::BoundsExceeded)?,
		};
		encoded.encode(w, version)?;
		u64::from(self.timescale).encode(w, version)?;
		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn info_sample() -> TrackInfo {
		TrackInfo {
			priority: 7,
			max_age: Some(Duration::from_millis(2000)),
			timescale: Timescale::MICRO,
		}
	}

	fn info_roundtrip(version: Version, info: &TrackInfo) -> TrackInfo {
		let mut buf = Vec::new();
		info.encode_msg(&mut buf, version).unwrap();
		let mut slice = buf.as_slice();
		TrackInfo::decode_msg(&mut slice, version).unwrap()
	}

	#[test]
	fn optional_max_age_roundtrips() {
		for version in [Version::Lite05, Version::Lite06, Version::Lite07] {
			for max_age in [None, Some(Duration::ZERO), Some(Duration::from_secs(30))] {
				let info = TrackInfo {
					max_age,
					..info_sample()
				};
				assert_eq!(info_roundtrip(version, &info).max_age, max_age);
			}
		}
	}

	#[test]
	fn lite07_reserves_zero_for_none_and_offsets_finite_ages() {
		for (age, encoded) in [
			(None, 0),
			(Some(Duration::ZERO), 1),
			(Some(Duration::from_millis(10)), 11),
		] {
			let mut buf = Vec::new();
			TrackInfo {
				max_age: age,
				..info_sample()
			}
			.encode_msg(&mut buf, Version::Lite07)
			.unwrap();
			assert_eq!(buf[1], encoded);
		}
	}

	#[test]
	fn legacy_unlimited_range_stays_safe_for_old_readers() {
		for version in [Version::Lite05, Version::Lite06] {
			for millis in [LEGACY_UNLIMITED - 1, LEGACY_UNLIMITED, 1 << 53, 1 << 60, (1 << 62) - 1] {
				let mut raw = Vec::new();
				0u8.encode(&mut raw, version).unwrap();
				super::super::subscribe::pad_group_order(&mut raw, version).unwrap();
				millis.encode(&mut raw, version).unwrap();
				1000u64.encode(&mut raw, version).unwrap();
				let decoded = TrackInfo::decode_msg(&mut raw.as_slice(), version).unwrap();
				assert_eq!(
					decoded.max_age,
					(millis < LEGACY_UNLIMITED).then(|| Duration::from_millis(millis))
				);
				let info = TrackInfo {
					max_age: Some(Duration::from_millis(millis)),
					..info_sample()
				};
				let mut encoded = Vec::new();
				info.encode_msg(&mut encoded, version).unwrap();
				let mut old_reader = encoded.as_slice();
				u8::decode(&mut old_reader, version).unwrap();
				super::super::subscribe::skip_group_order(&mut old_reader, version).unwrap();
				assert_eq!(
					u64::decode(&mut old_reader, version).unwrap(),
					millis.min(LEGACY_UNLIMITED)
				);
			}
		}
	}

	#[test]
	fn track_info_roundtrips_on_lite05() {
		let got = info_roundtrip(Version::Lite05, &info_sample());
		assert_eq!(got.priority, 7);
		assert_eq!(got.max_age, Some(Duration::from_millis(2000)));
		assert_eq!(got.timescale, Timescale::MICRO);
	}

	#[test]
	fn track_info_default_timescale_roundtrips() {
		let mut info = info_sample();
		info.timescale = Timescale::default();
		assert_eq!(info_roundtrip(Version::Lite05, &info).timescale, Timescale::MILLI);
	}

	#[test]
	fn track_info_defaults_match_cross_language_wire_bytes() {
		let info = crate::track::Info::default();
		let info = TrackInfo {
			priority: info.priority,
			max_age: info.max_age,
			timescale: info.timescale,
		};
		let mut buf = Vec::new();
		info.encode(&mut buf, Version::Lite05).unwrap();

		assert_eq!(
			buf,
			[
				0x0c, 0x00, 0x00, 0xc0, 0x1f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x43, 0xe8
			]
		);
	}

	#[test]
	fn track_info_errors_before_lite05() {
		let mut buf = Vec::new();
		assert!(info_sample().encode_msg(&mut buf, Version::Lite04).is_err());
	}

	#[test]
	fn track_info_roundtrips_varint_and_priority_bounds() {
		let info = TrackInfo {
			priority: 255,
			max_age: None,
			timescale: Timescale::new((1u64 << 62) - 1).unwrap(),
		};
		let got = info_roundtrip(Version::Lite05, &info);
		assert_eq!(got.priority, 255);
		assert_eq!(got.max_age, info.max_age);
		assert_eq!(got.timescale, info.timescale);
	}

	#[test]
	fn track_info_encodes_sub_millisecond_max_age_as_zero() {
		let info = TrackInfo {
			priority: 0,
			max_age: Some(Duration::from_nanos(999_999)),
			timescale: Timescale::MILLI,
		};
		let got = info_roundtrip(Version::Lite05, &info);
		assert_eq!(got.max_age, Some(Duration::ZERO));
	}

	#[test]
	fn track_info_encode_rejects_max_age_past_varint_without_writing() {
		let info = TrackInfo {
			priority: 7,
			max_age: Some(Duration::from_millis(1u64 << 62)),
			timescale: Timescale::MILLI,
		};
		let mut buf = Vec::new();
		assert!(info.encode(&mut buf, Version::Lite07).is_err());
		assert!(buf.is_empty());
	}

	#[test]
	fn track_request_roundtrips_on_lite05() {
		let msg = Track {
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut buf, Version::Lite05).unwrap();
		let mut slice = buf.as_slice();
		let got = Track::decode_msg(&mut slice, Version::Lite05).unwrap();
		assert_eq!(got.broadcast, Path::new("room"));
		assert_eq!(got.track, "video");
	}

	#[test]
	fn track_request_errors_before_lite05() {
		let msg = Track {
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
		};
		let mut buf = Vec::new();
		assert!(msg.encode_msg(&mut buf, Version::Lite04).is_err());
	}
}
