use std::{borrow::Cow, time::Duration};

use crate::{
	Path, Timescale,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

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
		if !version.has_timestamps() {
			return Err(DecodeError::Version);
		}

		let broadcast = Path::decode(r, version)?;
		let track = Cow::<str>::decode(r, version)?;

		Ok(Self { broadcast, track })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_timestamps() {
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
	/// The publisher's group ordering preference (newest-first when `false`).
	pub ordered: bool,
	/// Publisher Max Latency: an upper bound on how long the publisher caches a
	/// non-latest group past the arrival of a newer one. Encoded as milliseconds.
	pub cache: Duration,
	/// Per-frame timestamp scale (units per second). Mandatory on Lite05+: every track
	/// is timed, so this is always a real scale on the wire (never zero).
	pub timescale: Timescale,
}

impl Message for TrackInfo {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_timestamps() {
			return Err(DecodeError::Version);
		}

		let priority = u8::decode(r, version)?;
		let ordered = u8::decode(r, version)? != 0;
		let cache = Duration::decode(r, version)?;
		let timescale = Timescale::new(u64::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)?;

		Ok(Self {
			priority,
			ordered,
			cache,
			timescale,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_timestamps() {
			return Err(EncodeError::Version);
		}

		self.priority.encode(w, version)?;
		(self.ordered as u8).encode(w, version)?;
		self.cache.encode(w, version)?;
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
			ordered: false,
			cache: Duration::from_millis(2000),
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
	fn track_info_roundtrips_on_lite05() {
		let got = info_roundtrip(Version::Lite05Wip, &info_sample());
		assert_eq!(got.priority, 7);
		assert!(!got.ordered);
		assert_eq!(got.cache, Duration::from_millis(2000));
		assert_eq!(got.timescale, Timescale::MICRO);
	}

	#[test]
	fn track_info_default_timescale_roundtrips() {
		let mut info = info_sample();
		info.timescale = Timescale::default();
		assert_eq!(info_roundtrip(Version::Lite05Wip, &info).timescale, Timescale::MILLI);
	}

	#[test]
	fn track_info_errors_before_lite05() {
		let mut buf = Vec::new();
		assert!(info_sample().encode_msg(&mut buf, Version::Lite04).is_err());
	}

	#[test]
	fn track_request_roundtrips_on_lite05() {
		let msg = Track {
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
		};
		let mut buf = Vec::new();
		msg.encode_msg(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = buf.as_slice();
		let got = Track::decode_msg(&mut slice, Version::Lite05Wip).unwrap();
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
