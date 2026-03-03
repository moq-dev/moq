use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode, EncodeError, Sizer},
};

use super::{Message, Version};

/// Sent by the subscriber to request all future objects for the given track.
///
/// Objects will use the provided ID instead of the full track name, to save bytes.
#[derive(Clone, Debug)]
pub struct Subscribe<'a> {
	pub id: u64,
	pub broadcast: Path<'a>,
	pub track: Cow<'a, str>,
	pub priority: u8,
	pub ordered: bool,
	pub max_latency: std::time::Duration,
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
}

impl Message for Subscribe<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let id = u64::decode(r, version)?;
		let broadcast = Path::decode(r, version)?;
		let track = Cow::<str>::decode(r, version)?;
		let priority = u8::decode(r, version)?;

		let (ordered, max_latency, start_group, end_group) = match version {
			Version::Lite03 => {
				let ordered = u8::decode(r, version)? != 0;
				let max_latency = std::time::Duration::decode(r, version)?;
				let start_group = Option::<u64>::decode(r, version)?;
				let end_group = Option::<u64>::decode(r, version)?;
				(ordered, max_latency, start_group, end_group)
			}
			Version::Lite01 | Version::Lite02 => (false, std::time::Duration::ZERO, None, None),
		};

		Ok(Self {
			id,
			broadcast,
			track,
			priority,
			ordered,
			max_latency,
			start_group,
			end_group,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.id.encode(w, version)?;
		self.broadcast.encode(w, version)?;
		self.track.encode(w, version)?;
		self.priority.encode(w, version)?;

		match version {
			Version::Lite03 => {
				(self.ordered as u8).encode(w, version)?;
				self.max_latency.encode(w, version)?;
				self.start_group.encode(w, version)?;
				self.end_group.encode(w, version)?;
			}
			Version::Lite01 | Version::Lite02 => {}
		}

		Ok(())
	}
}

#[derive(Clone, Debug)]
pub struct SubscribeOk {
	pub priority: u8,
	pub ordered: bool,
	pub max_latency: std::time::Duration,
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
}

impl Message for SubscribeOk {
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite03 => {
				self.priority.encode(w, version)?;
				(self.ordered as u8).encode(w, version)?;
				self.max_latency.encode(w, version)?;
				self.start_group.encode(w, version)?;
				self.end_group.encode(w, version)?;
			}
			Version::Lite01 => {
				self.priority.encode(w, version)?;
			}
			Version::Lite02 => {}
		}

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite03 => {
				let priority = u8::decode(r, version)?;
				let ordered = u8::decode(r, version)? != 0;
				let max_latency = std::time::Duration::decode(r, version)?;
				let start_group = Option::<u64>::decode(r, version)?;
				let end_group = Option::<u64>::decode(r, version)?;

				Ok(Self {
					priority,
					ordered,
					max_latency,
					start_group,
					end_group,
				})
			}
			Version::Lite01 => Ok(Self {
				priority: u8::decode(r, version)?,
				ordered: false,
				max_latency: std::time::Duration::ZERO,
				start_group: None,
				end_group: None,
			}),
			Version::Lite02 => Ok(Self {
				priority: 0,
				ordered: false,
				max_latency: std::time::Duration::ZERO,
				start_group: None,
				end_group: None,
			}),
		}
	}
}

/// Sent by the subscriber to update subscription parameters.
///
/// Draft03 only.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct SubscribeUpdate {
	pub priority: u8,
	pub ordered: bool,
	pub max_latency: std::time::Duration,
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
}

impl Message for SubscribeUpdate {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
		}

		let priority = u8::decode(r, version)?;
		let ordered = u8::decode(r, version)? != 0;
		let max_latency = std::time::Duration::decode(r, version)?;
		let start_group = match u64::decode(r, version)? {
			0 => None,
			group => Some(group - 1),
		};
		let end_group = match u64::decode(r, version)? {
			0 => None,
			group => Some(group - 1),
		};

		Ok(Self {
			priority,
			ordered,
			max_latency,
			start_group,
			end_group,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
		}

		self.priority.encode(w, version)?;
		(self.ordered as u8).encode(w, version)?;
		self.max_latency.encode(w, version)?;

		match self.start_group {
			Some(start_group) => start_group
				.checked_add(1)
				.ok_or(EncodeError::TooLarge)?
				.encode(w, version)?,
			None => 0u64.encode(w, version)?,
		}

		match self.end_group {
			Some(end_group) => end_group
				.checked_add(1)
				.ok_or(EncodeError::TooLarge)?
				.encode(w, version)?,
			None => 0u64.encode(w, version)?,
		}

		Ok(())
	}
}

/// Indicates that one or more groups have been dropped.
///
/// The range `[start, end]` is inclusive on both ends. For example,
/// `start = 5, end = 7` means groups 5, 6, and 7 were dropped.
///
/// Draft03 only.
#[derive(Clone, Debug)]
pub struct SubscribeDrop {
	/// The first absolute group sequence in the dropped range.
	pub start: u64,

	/// The last absolute group sequence in the dropped range (inclusive).
	pub end: u64,

	/// An application-specific error code. A value of 0 indicates no error;
	/// the groups are simply unavailable.
	pub error: u64,
}

impl Message for SubscribeDrop {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
		}

		Ok(Self {
			start: u64::decode(r, version)?,
			end: u64::decode(r, version)?,
			error: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite03 => {}
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
		}

		self.start.encode(w, version)?;
		self.end.encode(w, version)?;
		self.error.encode(w, version)?;

		Ok(())
	}
}

/// A response message on the subscribe stream.
///
/// In Draft03, each response is prefixed with a type discriminator:
/// - 0x0 for SUBSCRIBE_OK
/// - 0x1 for SUBSCRIBE_DROP
///
/// SUBSCRIBE_OK must be the first message on the response stream.
#[derive(Clone, Debug)]
pub enum SubscribeResponse {
	Ok(SubscribeOk),
	Drop(SubscribeDrop),
}

impl Encode<Version> for SubscribeResponse {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite03 => match self {
				Self::Ok(ok) => {
					0u64.encode(w, version)?;
					// Write size-prefixed body using Message trait
					let mut sizer = Sizer::default();
					Message::encode_msg(ok, &mut sizer, version)?;
					sizer.size.encode(w, version)?;
					Message::encode_msg(ok, w, version)?;
				}
				Self::Drop(drop) => {
					1u64.encode(w, version)?;
					let mut sizer = Sizer::default();
					Message::encode_msg(drop, &mut sizer, version)?;
					sizer.size.encode(w, version)?;
					Message::encode_msg(drop, w, version)?;
				}
			},
			Version::Lite01 | Version::Lite02 => match self {
				Self::Ok(ok) => {
					let mut sizer = Sizer::default();
					Message::encode_msg(ok, &mut sizer, version)?;
					sizer.size.encode(w, version)?;
					Message::encode_msg(ok, w, version)?;
				}
				Self::Drop(_) => {
					return Err(EncodeError::Version);
				}
			},
		}

		Ok(())
	}
}

impl Decode<Version> for SubscribeResponse {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite03 => {
				let typ = u64::decode(buf, version)?;
				match typ {
					0 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
					1 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
			Version::Lite01 | Version::Lite02 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
		}
	}
}
