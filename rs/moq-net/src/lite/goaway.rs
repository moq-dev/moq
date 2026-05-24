use std::borrow::Cow;

use crate::coding::*;

use super::{Message, Version};

/// Sent to gracefully shut down a session and optionally redirect to a new URI.
///
/// Lite04+ only.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Goaway<'a> {
	pub uri: Cow<'a, str>,
}

impl Message for Goaway<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		let uri = Cow::<str>::decode(r, version)?;
		Ok(Self { uri })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.uri.encode(w, version)?;
		Ok(())
	}
}
