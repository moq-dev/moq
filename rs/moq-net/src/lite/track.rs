use std::borrow::Cow;

use crate::{
	Compression, Path, Timescale,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

/// Sent by the subscriber to open a Track Stream (0x6), requesting a track's
/// immutable publisher properties without subscribing or fetching.
///
/// The publisher replies with a single [`TrackInfo`] and then FINs the stream,
/// or resets it on error (e.g. the track does not exist). Lite05+ only.
#[derive(Clone, Debug)]
pub struct Track<'a> {
	pub broadcast: Path<'a>,
	pub track: Cow<'a, str>,
}

impl Message for Track<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let broadcast = Path::decode(r, version)?;
		let track = Cow::<str>::decode(r, version)?;

		Ok(Self { broadcast, track })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.broadcast.encode(w, version)?;
		self.track.encode(w, version)?;

		Ok(())
	}
}

/// Sent by the publisher in response to a [`Track`] request, carrying the track's
/// immutable publisher properties. It is the sole message on the Track Stream; the
/// publisher FINs immediately afterward, or resets the stream on error.
///
/// Every field is fixed for the lifetime of the track. Fetched once and cached by
/// the subscriber, so the properties are no longer echoed on every SUBSCRIBE/FETCH
/// response. Lite05+ only.
#[derive(Clone, Debug)]
pub struct TrackInfo {
	/// The publisher's priority for this track, used only to resolve ties between
	/// subscriptions of equal subscriber priority.
	pub priority: u8,
	/// The publisher's group ordering preference, used only to resolve ties.
	pub ordered: bool,
	/// How long the publisher keeps old groups available before evicting them. A
	/// relay re-serves with the same window and clamps each subscriber's stale
	/// preference to it.
	pub cache: std::time::Duration,
	/// Per-frame timestamp scale. `None` (wire `0`) means the publisher doesn't
	/// carry per-frame timestamps, so frame headers omit them.
	pub timescale: Option<Timescale>,
	/// Codec applied to every frame payload on this track. The subscriber needs
	/// this (and `timescale`) before it can decode any frame.
	pub compression: Compression,
}

impl Message for TrackInfo {
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		// Order matches draft-lcurley-moq-lite-05 TRACK_INFO: Priority, Ordered,
		// Cache, Timescale, Compression.
		self.priority.encode(w, version)?;
		(self.ordered as u8).encode(w, version)?;
		self.cache.encode(w, version)?;
		self.timescale.map(u64::from).unwrap_or(0).encode(w, version)?;
		self.compression.to_code().encode(w, version)?;

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let priority = u8::decode(r, version)?;
		let ordered = u8::decode(r, version)? != 0;
		let cache = std::time::Duration::decode(r, version)?;
		let timescale = Timescale::new(u64::decode(r, version)?).ok();
		let compression = Compression::from_code(u64::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)?;

		Ok(Self {
			priority,
			ordered,
			cache,
			timescale,
			compression,
		})
	}
}

#[cfg(test)]
mod test {
	use std::time::Duration;

	use super::*;

	fn sample() -> TrackInfo {
		TrackInfo {
			priority: 7,
			ordered: true,
			cache: Duration::from_secs(10),
			timescale: Some(Timescale::MICRO),
			compression: Compression::Deflate,
		}
	}

	fn roundtrip(info: &TrackInfo) -> TrackInfo {
		let mut buf = Vec::new();
		info.encode_msg(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = buf.as_slice();
		TrackInfo::decode_msg(&mut slice, Version::Lite05Wip).unwrap()
	}

	#[test]
	fn track_info_roundtrips() {
		let got = roundtrip(&sample());
		assert_eq!(got.priority, 7);
		assert!(got.ordered);
		assert_eq!(got.cache, Duration::from_secs(10));
		assert_eq!(got.timescale, Some(Timescale::MICRO));
		assert_eq!(got.compression, Compression::Deflate);
	}

	#[test]
	fn timescale_zero_decodes_as_none() {
		let mut info = sample();
		info.timescale = None;
		assert_eq!(roundtrip(&info).timescale, None);
	}

	#[test]
	fn rejected_before_lite05() {
		let mut buf = Vec::new();
		assert!(matches!(
			sample().encode_msg(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn track_request_roundtrips() {
		let req = Track {
			broadcast: Path::new("room/1"),
			track: Cow::Borrowed("video"),
		};
		let mut buf = Vec::new();
		req.encode_msg(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = buf.as_slice();
		let got = Track::decode_msg(&mut slice, Version::Lite05Wip).unwrap();
		assert_eq!(got.broadcast, Path::new("room/1"));
		assert_eq!(got.track, "video");
	}
}
