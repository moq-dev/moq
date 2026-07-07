use std::borrow::Cow;

use crate::{
	Path,
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

		Ok(Self {
			broadcast,
			track,
			priority,
			group,
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
		Ok(())
	}
}

#[cfg(test)]
mod test {
	use super::*;

	fn fetch_sample() -> Fetch<'static> {
		Fetch {
			broadcast: Path::new("room").to_owned(),
			track: Cow::Borrowed("video"),
			priority: 3,
			group: 7,
		}
	}

	fn fetch_roundtrip(version: Version, msg: &Fetch<'_>) -> Fetch<'static> {
		let mut buf = Vec::new();
		msg.encode_msg(&mut buf, version).unwrap();
		let mut slice = buf.as_slice();
		Fetch::decode_msg(&mut slice, version).unwrap()
	}

	#[test]
	fn fetch_roundtrips() {
		for version in [Version::Lite03, Version::Lite04, Version::Lite05] {
			let got = fetch_roundtrip(version, &fetch_sample());
			assert_eq!(got.broadcast, Path::new("room"));
			assert_eq!(got.track, "video");
			assert_eq!(got.priority, 3);
			assert_eq!(got.group, 7);
		}
	}

	#[test]
	fn fetch_rejected_before_lite03() {
		let mut buf = Vec::new();
		assert!(fetch_sample().encode_msg(&mut buf, Version::Lite02).is_err());
	}
}
