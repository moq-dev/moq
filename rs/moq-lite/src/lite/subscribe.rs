use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode},
	lite::{Message, Version},
};

/// Sent by the subscriber to request all future objects for the given track.
///
/// Objects will use the provided ID instead of the full track name, to save bytes.
#[derive(Clone, Debug)]
pub struct Subscribe<'a> {
	pub id: u64,
	pub broadcast: Path<'a>,
	pub track: Cow<'a, str>,
	pub priority: u8,
	pub delivery_timeout: Option<u64>,
}

impl Message for Subscribe<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let id = u64::decode(r, version)?;
		let broadcast = Path::decode(r, version)?;
		let track = Cow::<str>::decode(r, version)?;
		let priority = u8::decode(r, version)?;
		let delivery_timeout = if u8::decode(r, version)? == 1 {
			Some(u64::decode(r, version)?)
		} else {
			None
		};

		Ok(Self {
			id,
			broadcast,
			track,
			priority,
			delivery_timeout,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.id.encode(w, version);
		self.broadcast.encode(w, version);
		self.track.encode(w, version);
		self.priority.encode(w, version);
		if let Some(timeout) = self.delivery_timeout {
			1u8.encode(w, version);
			timeout.encode(w, version);
		} else {
			0u8.encode(w, version);
		}
	}
}

#[derive(Clone, Debug)]
pub struct SubscribeOk {
	pub priority: u8,
}

impl Message for SubscribeOk {
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		if version == Version::Draft01 {
			self.priority.encode(w, version);
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let priority = if version == Version::Draft01 {
			u8::decode(r, version)?
		} else {
			0
		};

		Ok(Self { priority })
	}
}
