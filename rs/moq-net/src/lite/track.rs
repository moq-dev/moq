use std::borrow::Cow;

use crate::{
	Compression, Path, Timescale,
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
	/// Per-frame timestamp scale, or `None` if frames carry no timestamps. On the
	/// wire `None` is `0` and `Some(n)` is `n`.
	pub timescale: Option<Timescale>,
	/// The algorithm the sender compressed this track's frame payloads with, or
	/// `None` for plaintext. On the wire `None` is `0` and an algorithm is its code.
	/// It MUST be one the receiver advertised as a decoder in SETUP.
	pub compression: Option<Compression>,
}

impl Message for TrackInfo {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_timestamps() {
			return Err(DecodeError::Version);
		}

		let priority = u8::decode(r, version)?;
		let ordered = u8::decode(r, version)? != 0;
		let timescale = Timescale::new(u64::decode(r, version)?).ok();
		// `0` is plaintext; any other code is an algorithm and MUST decode (an unknown
		// code means the sender used something we never advertised: a protocol error).
		let compression = match u64::decode(r, version)? {
			0 => None,
			code => Some(Compression::from_code(code).map_err(|_| DecodeError::InvalidValue)?),
		};

		Ok(Self {
			priority,
			ordered,
			timescale,
			compression,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_timestamps() {
			return Err(EncodeError::Version);
		}

		self.priority.encode(w, version)?;
		(self.ordered as u8).encode(w, version)?;
		self.timescale.map(u64::from).unwrap_or(0).encode(w, version)?;
		self.compression
			.map(Compression::to_code)
			.unwrap_or(0)
			.encode(w, version)?;
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
			timescale: Some(Timescale::MICRO),
			compression: Some(Compression::Deflate),
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
		assert_eq!(got.timescale, Some(Timescale::MICRO));
		assert_eq!(got.compression, Some(Compression::Deflate));
	}

	#[test]
	fn track_info_compression_variants_roundtrip() {
		for compression in [None, Some(Compression::Deflate), Some(Compression::Zstd)] {
			let mut info = info_sample();
			info.compression = compression;
			assert_eq!(info_roundtrip(Version::Lite05Wip, &info).compression, compression);
		}
	}

	#[test]
	fn track_info_rejects_unknown_compression_code() {
		// Hand-frame a TRACK_INFO with an unknown algorithm code (9) in the
		// compression slot: priority, ordered, timescale(0), compression(9).
		let mut buf = Vec::new();
		7u8.encode(&mut buf, Version::Lite05Wip).unwrap();
		0u8.encode(&mut buf, Version::Lite05Wip).unwrap();
		0u64.encode(&mut buf, Version::Lite05Wip).unwrap();
		9u64.encode(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = buf.as_slice();
		assert!(TrackInfo::decode_msg(&mut slice, Version::Lite05Wip).is_err());
	}

	#[test]
	fn track_info_timescale_none_roundtrips() {
		let mut info = info_sample();
		info.timescale = None;
		assert_eq!(info_roundtrip(Version::Lite05Wip, &info).timescale, None);
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
