use std::borrow::Cow;

use crate::{
	coding::{Decode, DecodeError, Encode},
	lite::{Message, Version},
	Path,
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
	pub max_latency: std::time::Duration,
	pub version: Version,
}

impl<'a> Message for Subscribe<'a> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let id = u64::decode(r, version)?;
		let broadcast = Path::decode(r, version)?;
		let track = Cow::<str>::decode(r, version)?;
		let priority = u8::decode(r, version)?;

		let max_latency = match version {
			Version::Draft01 | Version::Draft02 => std::time::Duration::default(),
			Version::Draft03 => std::time::Duration::from_millis(u64::decode(r, version)?),
		};

		Ok(Self {
			id,
			broadcast,
			track,
			priority,
			max_latency,
			version,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.id.encode(w, version);
		self.broadcast.encode(w, version);
		self.track.encode(w, version);
		self.priority.encode(w, version);

		match version {
			Version::Draft01 | Version::Draft02 => {}
			Version::Draft03 => {
				let max_latency: u64 = self.max_latency.as_millis().try_into().expect("duration too large");
				max_latency.encode(w, version);
			}
		}
	}
}

#[derive(Clone, Debug)]
pub struct SubscribeOk {
	pub priority: u8,
}

impl Message for SubscribeOk {
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft01 => self.priority.encode(w, version),
			Version::Draft02 | Version::Draft03 => {}
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let priority = match version {
			Version::Draft01 => u8::decode(r, version)?,
			Version::Draft02 | Version::Draft03 => 0,
		};

		Ok(Self { priority })
	}
}

pub struct SubscribeUpdate {
	pub priority: u8,
	pub max_latency: std::time::Duration,
}

impl Message for SubscribeUpdate {
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.priority.encode(w, version);

		match version {
			Version::Draft01 | Version::Draft02 => {}
			Version::Draft03 => {
				let max_latency: u64 = self.max_latency.as_millis().try_into().expect("duration too large");
				max_latency.encode(w, version);
			}
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let priority = u8::decode(r, version)?;

		let max_latency = match version {
			Version::Draft01 | Version::Draft02 => std::time::Duration::default(),
			Version::Draft03 => std::time::Duration::from_millis(u64::decode(r, version)?),
		};

		Ok(Self { priority, max_latency })
	}
}
