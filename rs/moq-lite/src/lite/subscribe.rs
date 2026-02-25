use std::borrow::Cow;

use crate::{
	Path,
	coding::{Decode, DecodeError, Encode, Sizer},
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
			Version::Draft03 => {
				let ordered = u8::decode(r, version)? != 0;
				let max_latency = std::time::Duration::decode(r, version)?;
				let start_group = Option::<u64>::decode(r, version)?;
				let end_group = Option::<u64>::decode(r, version)?;
				(ordered, max_latency, start_group, end_group)
			}
			Version::Draft01 | Version::Draft02 => (false, std::time::Duration::ZERO, None, None),
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

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.id.encode(w, version);
		self.broadcast.encode(w, version);
		self.track.encode(w, version);
		self.priority.encode(w, version);

		match version {
			Version::Draft03 => {
				(self.ordered as u8).encode(w, version);
				self.max_latency.encode(w, version);
				self.start_group.encode(w, version);
				self.end_group.encode(w, version);
			}
			Version::Draft01 | Version::Draft02 => {}
		}
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
	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft03 => {
				self.priority.encode(w, version);
				(self.ordered as u8).encode(w, version);
				self.max_latency.encode(w, version);
				self.start_group.encode(w, version);
				self.end_group.encode(w, version);
			}
			Version::Draft01 => {
				self.priority.encode(w, version);
			}
			Version::Draft02 => {}
		}
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft03 => {
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
			Version::Draft01 => Ok(Self {
				priority: u8::decode(r, version)?,
				ordered: false,
				max_latency: std::time::Duration::ZERO,
				start_group: None,
				end_group: None,
			}),
			Version::Draft02 => Ok(Self {
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
			Version::Draft01 | Version::Draft02 => {
				unreachable!("subscribe update not supported for version: {:?}", version);
			}
			Version::Draft03 => {}
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

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft01 | Version::Draft02 => {
				unreachable!("subscribe update not supported for version: {:?}", version);
			}
			Version::Draft03 => {}
		}

		self.priority.encode(w, version);
		(self.ordered as u8).encode(w, version);
		self.max_latency.encode(w, version);

		match self.start_group {
			Some(start_group) => (start_group + 1).encode(w, version),
			None => 0u64.encode(w, version),
		}

		match self.end_group {
			Some(end_group) => (end_group + 1).encode(w, version),
			None => 0u64.encode(w, version),
		}
	}
}

/// Indicates that one or more groups have been dropped.
///
/// Draft03 only.
#[derive(Clone, Debug)]
pub struct SubscribeDrop {
	pub sequence: u64,
	pub count: u64,
	pub error: u64,
}

impl Message for SubscribeDrop {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft01 | Version::Draft02 => {
				unreachable!("subscribe drop not supported for version: {:?}", version);
			}
			Version::Draft03 => {}
		}

		Ok(Self {
			sequence: u64::decode(r, version)?,
			count: u64::decode(r, version)?,
			error: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft01 | Version::Draft02 => {
				unreachable!("subscribe drop not supported for version: {:?}", version);
			}
			Version::Draft03 => {}
		}

		self.sequence.encode(w, version);
		self.count.encode(w, version);
		self.error.encode(w, version);
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
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft03 => match self {
				Self::Ok(ok) => {
					0u64.encode(w, version);
					// Write size-prefixed body using Message trait
					let mut sizer = Sizer::default();
					Message::encode_msg(ok, &mut sizer, version);
					sizer.size.encode(w, version);
					Message::encode_msg(ok, w, version);
				}
				Self::Drop(drop) => {
					1u64.encode(w, version);
					let mut sizer = Sizer::default();
					Message::encode_msg(drop, &mut sizer, version);
					sizer.size.encode(w, version);
					Message::encode_msg(drop, w, version);
				}
			},
			Version::Draft01 | Version::Draft02 => match self {
				Self::Ok(ok) => {
					let mut sizer = Sizer::default();
					Message::encode_msg(ok, &mut sizer, version);
					sizer.size.encode(w, version);
					Message::encode_msg(ok, w, version);
				}
				Self::Drop(_) => {
					unreachable!("subscribe drop not supported for version: {:?}", version);
				}
			},
		}
	}
}

impl Decode<Version> for SubscribeResponse {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Draft03 => {
				let typ = u64::decode(buf, version)?;
				match typ {
					0 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
					1 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
			Version::Draft01 | Version::Draft02 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
		}
	}
}
