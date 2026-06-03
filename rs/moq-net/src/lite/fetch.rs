use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

/// Sent by the subscriber to fetch a specific group from a track.
///
/// Lite03+ only.
#[allow(dead_code)]
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

#[cfg(test)]
mod tests {
	use super::*;

	fn sample() -> Fetch<'static> {
		Fetch {
			broadcast: Path::new("room/1"),
			track: Cow::Borrowed("video"),
			priority: 3,
			group: 42,
			frame_start: 7,
		}
	}

	fn roundtrip(version: Version, fetch: &Fetch<'_>) -> Fetch<'static> {
		let mut buf = Vec::new();
		fetch.encode_msg(&mut buf, version).unwrap();
		let mut r = buf.as_slice();
		Fetch::decode_msg(&mut r, version).unwrap()
	}

	#[test]
	fn frame_start_roundtrips_on_lite05() {
		let got = roundtrip(Version::Lite05Wip, &sample());
		assert_eq!(got.group, 42);
		assert_eq!(got.frame_start, 7);
	}

	#[test]
	fn frame_start_absent_before_lite05() {
		// Lite03/Lite04 don't carry the frame start varint, so it always decodes as 0.
		let got = roundtrip(Version::Lite04, &sample());
		assert_eq!(got.group, 42);
		assert_eq!(got.frame_start, 0);

		// The lite-04 encoding is strictly shorter (no trailing frame start varint).
		let mut buf04 = Vec::new();
		sample().encode_msg(&mut buf04, Version::Lite04).unwrap();
		let mut buf05 = Vec::new();
		sample().encode_msg(&mut buf05, Version::Lite05Wip).unwrap();
		assert!(buf05.len() > buf04.len(), "lite-05 carries an extra frame start varint");
	}
}
