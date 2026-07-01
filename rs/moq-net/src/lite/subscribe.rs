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

/// The publisher's positive response to a SUBSCRIBE.
///
/// In moq-lite-05 this is trimmed to the resolved absolute start group; the publisher's
/// per-track properties moved to [`TrackInfo`](super::TrackInfo). The resolved group lives
/// in `start_group` as a raw absolute sequence (not the `+1`-encoded form used in the
/// SUBSCRIBE request). Earlier versions carry the publisher properties inline.
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
			Version::Lite03 | Version::Lite04 => {
				self.priority.encode(w, version)?;
				(self.ordered as u8).encode(w, version)?;
				self.max_latency.encode(w, version)?;
				self.start_group.encode(w, version)?;
				self.end_group.encode(w, version)?;
			}
			// moq-lite-05+: just the resolved absolute start group (raw, 0 is valid).
			_ => {
				self.start_group.unwrap_or(0).encode(w, version)?;
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
			Version::Lite03 | Version::Lite04 => {
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
			// moq-lite-05+: just the resolved absolute start group.
			_ => Ok(Self {
				priority: 0,
				ordered: false,
				max_latency: std::time::Duration::ZERO,
				start_group: Some(u64::decode(r, version)?),
				end_group: None,
			}),
		}
	}
}

/// Sent by the publisher to signal that no group after a given sequence will be produced.
///
/// moq-lite-05+ only. Bounds the subscription range; the stream still FINs only once every
/// group up to this sequence has been accounted for.
#[derive(Clone, Debug)]
pub struct SubscribeEnd {
	/// The absolute sequence of the last group that may be delivered (inclusive).
	pub group: u64,
}

impl Message for SubscribeEnd {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		if !version.has_track_stream() {
			return Err(DecodeError::Version);
		}

		Ok(Self {
			group: u64::decode(r, version)?,
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		if !version.has_track_stream() {
			return Err(EncodeError::Version);
		}

		self.group.encode(w, version)?;
		Ok(())
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

/// A response message on the subscribe stream.
///
/// In Lite03/04 each response is prefixed with a type discriminator: 0x0 SUBSCRIBE_OK,
/// 0x1 SUBSCRIBE_DROP. In Lite05 end-of-subscription splits out, so the discriminators
/// become 0x0 SUBSCRIBE_OK, 0x1 SUBSCRIBE_END, 0x2 SUBSCRIBE_DROP.
///
/// SUBSCRIBE_OK is normally the first message on the response stream.
#[derive(Clone, Debug)]
pub enum SubscribeResponse {
	Ok(SubscribeOk),
	End(SubscribeEnd),
	Drop(SubscribeDrop),
}

/// Encode a size-prefixed message body (no type discriminator).
fn encode_body<W: bytes::BufMut, M: Message>(msg: &M, w: &mut W, version: Version) -> Result<(), EncodeError> {
	let mut sizer = Sizer::default();
	Message::encode_msg(msg, &mut sizer, version)?;
	sizer.size.encode(w, version)?;
	Message::encode_msg(msg, w, version)
}

impl Encode<Version> for SubscribeResponse {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			// No type discriminator; only SUBSCRIBE_OK exists.
			Version::Lite01 | Version::Lite02 => match self {
				Self::Ok(ok) => encode_body(ok, w, version)?,
				Self::End(_) | Self::Drop(_) => return Err(EncodeError::Version),
			},
			// 0x0 OK, 0x1 DROP; no SUBSCRIBE_END.
			Version::Lite03 | Version::Lite04 => match self {
				Self::Ok(ok) => {
					0u64.encode(w, version)?;
					encode_body(ok, w, version)?;
				}
				Self::Drop(drop) => {
					1u64.encode(w, version)?;
					encode_body(drop, w, version)?;
				}
				Self::End(_) => return Err(EncodeError::Version),
			},
			// moq-lite-05+: 0x0 OK, 0x1 END, 0x2 DROP.
			_ => match self {
				Self::Ok(ok) => {
					0u64.encode(w, version)?;
					encode_body(ok, w, version)?;
				}
				Self::End(end) => {
					1u64.encode(w, version)?;
					encode_body(end, w, version)?;
				}
				Self::Drop(drop) => {
					2u64.encode(w, version)?;
					encode_body(drop, w, version)?;
				}
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
					0 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
					1 => Ok(Self::End(SubscribeEnd::decode(buf, version)?)),
					2 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
					_ => Err(DecodeError::InvalidMessage(typ)),
				}
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use bytes::BytesMut;

	fn response_roundtrip(resp: &SubscribeResponse, version: Version) -> SubscribeResponse {
		let mut buf = BytesMut::new();
		resp.encode(&mut buf, version).unwrap();
		let mut slice = &buf[..];
		let decoded = SubscribeResponse::decode(&mut slice, version).unwrap();
		assert!(bytes::Buf::remaining(&slice) == 0, "trailing bytes after decode");
		decoded
	}

	#[test]
	fn lite05_subscribe_ok_resolved_group() {
		let resp = SubscribeResponse::Ok(SubscribeOk {
			priority: 0,
			ordered: false,
			max_latency: std::time::Duration::ZERO,
			start_group: Some(42),
			end_group: None,
		});
		match response_roundtrip(&resp, Version::Lite05Wip) {
			SubscribeResponse::Ok(ok) => assert_eq!(ok.start_group, Some(42)),
			other => panic!("expected Ok, got {other:?}"),
		}
	}

	#[test]
	fn lite05_subscribe_end() {
		let resp = SubscribeResponse::End(SubscribeEnd { group: 7 });
		match response_roundtrip(&resp, Version::Lite05Wip) {
			SubscribeResponse::End(end) => assert_eq!(end.group, 7),
			other => panic!("expected End, got {other:?}"),
		}
	}

	#[test]
	fn lite05_subscribe_drop() {
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 1,
			end: 3,
			error: 0,
		});
		match response_roundtrip(&resp, Version::Lite05Wip) {
			SubscribeResponse::Drop(d) => {
				assert_eq!(d.start, 1);
				assert_eq!(d.end, 3);
			}
			other => panic!("expected Drop, got {other:?}"),
		}
	}

	#[test]
	fn lite04_has_no_subscribe_end() {
		let resp = SubscribeResponse::End(SubscribeEnd { group: 5 });
		let mut buf = BytesMut::new();
		assert!(matches!(
			resp.encode(&mut buf, Version::Lite04),
			Err(EncodeError::Version)
		));
	}

	#[test]
	fn lite04_drop_uses_type_1() {
		// In lite-04 DROP is discriminator 1; lite-05 reassigns 1 to END.
		let resp = SubscribeResponse::Drop(SubscribeDrop {
			start: 2,
			end: 2,
			error: 9,
		});
		match response_roundtrip(&resp, Version::Lite04) {
			SubscribeResponse::Drop(d) => assert_eq!(d.error, 9),
			other => panic!("expected Drop, got {other:?}"),
		}
	}
}
