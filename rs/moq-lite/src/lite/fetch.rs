use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode},
	lite::{Message, Version},
};

/// Sent by the subscriber to fetch a specific group from a track.
///
/// Draft03 only.
#[allow(dead_code)]
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
			Version::Draft01 | Version::Draft02 => {
				unreachable!("fetch not supported for version: {:?}", version);
			}
			Version::Draft03 => {}
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

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft01 | Version::Draft02 => {
				unreachable!("fetch not supported for version: {:?}", version);
			}
			Version::Draft03 => {}
		}

		self.broadcast.encode(w, version);
		self.track.encode(w, version);
		self.priority.encode(w, version);
		self.group.encode(w, version);
	}
}
