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
			Version::Lite01 | Version::Lite02 => (false, std::time::Duration::ZERO, None, None),
			_ => {
				let ordered = u8::decode(r, version)? != 0;
				let max_latency = std::time::Duration::decode(r, version)?;
				let start_group = Option::<u64>::decode(r, version)?;
				let end_group = Option::<u64>::decode(r, version)?;
				(ordered, max_latency, start_group, end_group)
			}
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
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				(self.ordered as u8).encode(w, version)?;
				self.max_latency.encode(w, version)?;
				self.start_group.encode(w, version)?;
				self.end_group.encode(w, version)?;
			}
		}

		Ok(())
	}
}

/// Publisher's acknowledgement on the Subscribe Stream for drafts 01-04.
///
/// Lite05+ replaced this with implicit acceptance plus
/// [`SubscribeStart`]/[`SubscribeEnd`]; the immutable timescale/cache moved
/// to [`super::TrackInfo`].
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
			Version::Lite01 => {
				self.priority.encode(w, version)?;
			}
			Version::Lite02 => {}
			// Lite05+ never sends SUBSCRIBE_OK, but keep the field layout matching
			// Lite03/04 so a stray future use stays well-formed.
			_ => {
				self.priority.encode(w, version)?;
				(self.ordered as u8).encode(w, version)?;
				self.max_latency.encode(w, version)?;
				self.start_group.encode(w, version)?;
				self.end_group.encode(w, version)?;
			}
		}

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
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
			_ => {
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
		}
	}
}

/// Resolves the absolute start group of a Lite05+ subscription. The first message
/// the publisher sends, once the start group is known. A value greater than the
/// requested start implicitly drops the leading range.
#[derive(Clone, Debug)]
pub struct SubscribeStart {
	pub group: u64,
}

impl Message for SubscribeStart {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_timestamps() {
			return Err(DecodeError::Version);
		}
		Ok(Self {
			group: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_timestamps() {
			return Err(EncodeError::Version);
		}
		self.group.encode(w, version)
	}
}

/// Signals that no group after `group` (inclusive upper bound) will be produced on
/// a Lite05+ subscription. Bounds the range but doesn't end the stream: stragglers
/// at or below it may still be dropped before FIN.
#[derive(Clone, Debug)]
pub struct SubscribeEnd {
	pub group: u64,
}

impl Message for SubscribeEnd {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_timestamps() {
			return Err(DecodeError::Version);
		}
		Ok(Self {
			group: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_timestamps() {
			return Err(EncodeError::Version);
		}
		self.group.encode(w, version)
	}
}

/// Sent by the subscriber to update subscription parameters.
///
/// Lite03+ only.
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
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
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
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
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
/// Lite03+ only.
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
			Version::Lite01 | Version::Lite02 => {
				return Err(DecodeError::Version);
			}
			_ => {}
		}

		Ok(Self {
			start: u64::decode(r, version)?,
			end: u64::decode(r, version)?,
			error: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {
				return Err(EncodeError::Version);
			}
			_ => {}
		}

		self.start.encode(w, version)?;
		self.end.encode(w, version)?;
		self.error.encode(w, version)?;

		Ok(())
	}
}

/// A response message on the subscribe stream, prefixed with a type discriminator
/// on Lite03+.
///
/// The discriminator is version-dependent:
/// - Lite03/04: `0x0` SUBSCRIBE_OK, `0x1` SUBSCRIBE_DROP.
/// - Lite05+: `0x0` SUBSCRIBE_START, `0x1` SUBSCRIBE_END, `0x2` SUBSCRIBE_DROP
///   (SUBSCRIBE_OK was removed; acceptance is implicit).
#[derive(Clone, Debug)]
pub enum SubscribeResponse {
	Ok(SubscribeOk),
	Start(SubscribeStart),
	End(SubscribeEnd),
	Drop(SubscribeDrop),
}

/// Write a `type` varint followed by the size-prefixed message body.
fn encode_typed<W: bytes::BufMut, M: Message>(
	w: &mut W,
	typ: u64,
	msg: &M,
	version: Version,
) -> Result<(), EncodeError> {
	typ.encode(w, version)?;
	let mut sizer = Sizer::default();
	msg.encode_msg(&mut sizer, version)?;
	sizer.size.encode(w, version)?;
	msg.encode_msg(w, version)
}

impl Encode<Version> for SubscribeResponse {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => match self {
				Self::Ok(ok) => {
					let mut sizer = Sizer::default();
					Message::encode_msg(ok, &mut sizer, version)?;
					sizer.size.encode(w, version)?;
					Message::encode_msg(ok, w, version)?;
				}
				_ => return Err(EncodeError::Version),
			},
			Version::Lite03 | Version::Lite04 => match self {
				Self::Ok(ok) => encode_typed(w, 0, ok, version)?,
				Self::Drop(drop) => encode_typed(w, 1, drop, version)?,
				_ => return Err(EncodeError::Version),
			},
			// Lite05+: SUBSCRIBE_OK is gone; START/END/DROP carry the resolved range.
			_ => match self {
				Self::Start(start) => encode_typed(w, 0, start, version)?,
				Self::End(end) => encode_typed(w, 1, end, version)?,
				Self::Drop(drop) => encode_typed(w, 2, drop, version)?,
				Self::Ok(_) => return Err(EncodeError::Version),
			},
		}

		Ok(())
	}
}

impl Decode<Version> for SubscribeResponse {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
			Version::Lite03 | Version::Lite04 => {
				let typ = u64::decode(buf, version)?;
				match typ {
					0 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
					1 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
			_ => {
				let typ = u64::decode(buf, version)?;
				match typ {
					0 => Ok(Self::Start(SubscribeStart::decode(buf, version)?)),
					1 => Ok(Self::End(SubscribeEnd::decode(buf, version)?)),
					2 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
		}
	}
}

#[cfg(test)]
mod test {
	use super::*;

	#[test]
	fn subscribe_start_roundtrips_on_lite05() {
		let resp = SubscribeResponse::Start(SubscribeStart { group: 42 });
		let mut buf = Vec::new();
		resp.encode(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = buf.as_slice();
		match SubscribeResponse::decode(&mut slice, Version::Lite05Wip).unwrap() {
			SubscribeResponse::Start(start) => assert_eq!(start.group, 42),
			other => panic!("expected Start, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_end_roundtrips_on_lite05() {
		let resp = SubscribeResponse::End(SubscribeEnd { group: 7 });
		let mut buf = Vec::new();
		resp.encode(&mut buf, Version::Lite05Wip).unwrap();
		let mut slice = buf.as_slice();
		match SubscribeResponse::decode(&mut slice, Version::Lite05Wip).unwrap() {
			SubscribeResponse::End(end) => assert_eq!(end.group, 7),
			other => panic!("expected End, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_drop_is_type_2_on_lite05() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 1,
			end: 3,
			error: 0,
		});
		let mut buf = Vec::new();
		resp.encode(&mut buf, Version::Lite05Wip).unwrap();
		// Type discriminator is the first varint; on Lite05 DROP is 0x2.
		assert_eq!(buf[0], 2);

		let mut slice = buf.as_slice();
		match SubscribeResponse::decode(&mut slice, Version::Lite05Wip).unwrap() {
			SubscribeResponse::Drop(drop) => assert_eq!((drop.start, drop.end), (1, 3)),
			other => panic!("expected Drop, got {other:?}"),
		}
	}

	#[test]
	fn subscribe_drop_is_type_1_on_lite04() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 1,
			end: 3,
			error: 0,
		});
		let mut buf = Vec::new();
		resp.encode(&mut buf, Version::Lite04).unwrap();
		assert_eq!(buf[0], 1);
	}

	#[test]
	fn subscribe_ok_rejected_on_lite05() {
		let resp = SubscribeResponse::Ok(SubscribeOk {
			priority: 1,
			ordered: true,
			max_latency: std::time::Duration::ZERO,
			start_group: None,
			end_group: None,
		});
		let mut buf = Vec::new();
		assert!(resp.encode(&mut buf, Version::Lite05Wip).is_err());
	}
}
