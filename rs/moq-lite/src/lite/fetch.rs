use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode, EncodeError},
};

use super::{Message, Version};

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
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
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
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
		}

		self.broadcast.encode(w, version)?;
		self.track.encode(w, version)?;
		self.priority.encode(w, version)?;
		self.group.encode(w, version)?;
		Ok(())
	}
}
