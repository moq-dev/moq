use std::borrow::Cow;
use std::time::Duration;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

/// Sent by a subscriber on a Track stream (moq-lite-05+) to request a track's
/// immutable publisher properties without subscribing or fetching.
#[derive(Clone, Debug)]
pub struct Track<'a> {
	/// The broadcast path of the track.
	pub broadcast: Path<'a>,
	/// The name of the track within the broadcast.
	pub track: Cow<'a, str>,
}

impl Message for Track<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}

		Ok(Self {
			broadcast: Path::decode(r, version)?,
			track: Cow::<str>::decode(r, version)?,
		})
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

/// The publisher's reply on a Track stream (moq-lite-05+): the track's immutable
/// properties. Sent once, then the publisher FINs the stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
	/// The publisher's priority for this track, used only to break ties.
	pub priority: u8,
	/// The publisher's group ordering preference (ascending if true), used only to break ties.
	pub ordered: bool,
	/// The longest the publisher caches a non-latest group past the arrival of a newer one.
	pub max_latency: Duration,
	/// Timestamp units per second for this track's frame timestamps. Always non-zero.
	pub timescale: u64,
}

impl Message for TrackInfo {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}

		let priority = u8::decode(r, version)?;
		let ordered = u8::decode(r, version)? != 0;
		let max_latency = Duration::decode(r, version)?;
		let timescale = u64::decode(r, version)?;

		// A zero timescale is a protocol violation: every track has a media timeline.
		if timescale == 0 {
			return Err(DecodeError::InvalidValue);
		}

		Ok(Self {
			priority,
			ordered,
			max_latency,
			timescale,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}

		if self.timescale == 0 {
			return Err(EncodeError::InvalidState);
		}

		self.priority.encode(w, version)?;
		(self.ordered as u8).encode(w, version)?;
		self.max_latency.encode(w, version)?;
		self.timescale.encode(w, version)?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::coding::{Decode, Encode};
	use bytes::BytesMut;

	fn roundtrip(info: &TrackInfo) -> TrackInfo {
		let mut buf = BytesMut::new();
		info.encode(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = &buf[..];
		let decoded = TrackInfo::decode(&mut slice, Version::Lite05Wip).unwrap();
		assert!(bytes::Buf::remaining(&slice) == 0, "trailing bytes after decode");
		decoded
	}

	#[test]
	fn track_info_roundtrip() {
		let info = TrackInfo {
			priority: 7,
			ordered: true,
			max_latency: Duration::from_millis(2500),
			timescale: 1000,
		};
		assert_eq!(roundtrip(&info), info);
	}

	#[test]
	fn rejects_zero_timescale() {
		let mut buf = BytesMut::new();
		assert!(matches!(
			TrackInfo {
				priority: 0,
				ordered: false,
				max_latency: Duration::ZERO,
				timescale: 0,
			}
			.encode(&mut buf, Version::Lite05Wip),
			Err(EncodeError::InvalidState)
		));
	}

	#[test]
	fn track_roundtrip() {
		let track = Track {
			broadcast: Path::new("room/1"),
			track: Cow::Borrowed("video"),
		};
		let mut buf = BytesMut::new();
		track.encode(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = &buf[..];
		let decoded = Track::decode(&mut slice, Version::Lite05Wip).unwrap();
		assert!(bytes::Buf::remaining(&slice) == 0, "trailing bytes after decode");
		assert_eq!(decoded.broadcast, track.broadcast);
		assert_eq!(decoded.track, track.track);
	}

	#[test]
	fn rejects_before_lite05() {
		let mut buf = BytesMut::new();
		assert!(matches!(
			TrackInfo {
				priority: 0,
				ordered: false,
				max_latency: Duration::ZERO,
				timescale: 1000,
			}
			.encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}
}
