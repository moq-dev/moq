use std::borrow::Cow;

use crate::{
	Compression, Path, Timescale,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

/// Sent by the subscriber to fetch a specific group from a track.
///
/// Lite03+ only.
#[derive(Clone, Debug)]
pub struct Fetch<'a> {
	pub broadcast: Path<'a>,
	pub track: Cow<'a, str>,
	pub priority: u8,
	pub group: u64,
	/// The 0-based index of the first frame to return; the publisher skips all
	/// earlier frames. `0` returns the entire group. Lite05+ only; older drafts
	/// always return the whole group.
	pub frame_start: u64,
}

impl Message for Fetch<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let broadcast = Path::decode(r, version)?;
		let track = Cow::<str>::decode(r, version)?;
		let priority = u8::decode(r, version)?;
		let group = u64::decode(r, version)?;
		let frame_start = match version {
			Version::Lite03 | Version::Lite04 => 0,
			_ => u64::decode(r, version)?,
		};

		Ok(Self {
			broadcast,
			track,
			priority,
			group,
			frame_start,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.broadcast.encode(w, version)?;
		self.track.encode(w, version)?;
		self.priority.encode(w, version)?;
		self.group.encode(w, version)?;
		match version {
			Version::Lite03 | Version::Lite04 => {}
			_ => self.frame_start.encode(w, version)?,
		}
		Ok(())
	}
}

/// Publisher's response to a [`Fetch`], sent on the same bidirectional stream
/// immediately before the group's frames.
///
/// Mirrors the codec/timescale negotiation in [`super::SubscribeOk`] so the
/// subscriber can decode the frames that follow. There is no error variant: a
/// failed fetch resets the stream instead. Lite05+ only.
#[derive(Clone, Debug)]
pub struct FetchOk {
	/// Echo of the requested group sequence, for a sanity check.
	pub group: u64,
	/// Codec the publisher used for every frame in this group.
	pub compression: Compression,
	/// Per-frame timestamp scale, or `None` if the frames carry no timestamps.
	/// On the wire `None` is `0` and `Some(n)` is `n`.
	pub timescale: Option<Timescale>,
}

impl Message for FetchOk {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let group = u64::decode(r, version)?;
		let timescale = Timescale::new(u64::decode(r, version)?).ok();
		let compression = Compression::from_code(u64::decode(r, version)?).map_err(|_| DecodeError::InvalidValue)?;

		Ok(Self {
			group,
			compression,
			timescale,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 | Version::Lite04 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.group.encode(w, version)?;
		self.timescale.map(u64::from).unwrap_or(0).encode(w, version)?;
		self.compression.to_code().encode(w, version)?;
		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn sample() -> FetchOk {
		FetchOk {
			group: 42,
			compression: Compression::Deflate,
			timescale: Some(Timescale::MICRO),
		}
	}

	fn roundtrip(version: Version, ok: &FetchOk) -> FetchOk {
		let mut buf = Vec::new();
		ok.encode_msg(&mut buf, version).unwrap();
		let mut slice = buf.as_slice();
		FetchOk::decode_msg(&mut slice, version).unwrap()
	}

	#[test]
	fn fetch_ok_roundtrips_on_lite05() {
		let got = roundtrip(Version::Lite05Wip, &sample());
		assert_eq!(got.group, 42);
		assert_eq!(got.compression, Compression::Deflate);
		assert_eq!(got.timescale, Some(Timescale::MICRO));
	}

	#[test]
	fn fetch_ok_timescale_none_roundtrips() {
		let mut ok = sample();
		ok.timescale = None;
		assert_eq!(roundtrip(Version::Lite05Wip, &ok).timescale, None);
	}

	#[test]
	fn fetch_ok_errors_before_lite05() {
		let mut buf = Vec::new();
		assert!(sample().encode_msg(&mut buf, Version::Lite04).is_err());
	}

	fn fetch_sample() -> Fetch<'static> {
		Fetch {
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
			priority: 3,
			group: 7,
			frame_start: 5,
		}
	}

	fn fetch_roundtrip(version: Version, msg: &Fetch<'_>) -> Fetch<'static> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut buf, version).unwrap();
		let mut slice = buf.as_slice();
		Fetch::decode_msg(&mut slice, version).unwrap()
	}

	#[test]
	fn fetch_frame_start_roundtrips_on_lite05() {
		assert_eq!(fetch_roundtrip(Version::Lite05Wip, &fetch_sample()).frame_start, 5);
	}

	#[test]
	fn fetch_frame_start_absent_before_lite05() {
		let msg = fetch_sample();

		// The frame_start varint only exists on lite-05+, so the older encoding is
		// strictly shorter and always decodes back as 0.
		let mut buf04 = Vec::new();
		msg.encode_msg(&mut buf04, Version::Lite04).unwrap();
		let mut buf05 = Vec::new();
		msg.encode_msg(&mut buf05, Version::Lite05Wip).unwrap();
		assert!(
			buf05.len() > buf04.len(),
			"lite-05 carries the extra frame_start varint"
		);

		assert_eq!(fetch_roundtrip(Version::Lite04, &msg).frame_start, 0);
	}
}
