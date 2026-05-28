use std::borrow::Cow;

use crate::{
	Path, Timescale,
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

#[derive(Clone, Debug, Default)]
pub struct SubscribeOk {
	pub priority: u8,
	pub ordered: bool,
	pub max_latency: std::time::Duration,
	pub start_group: Option<u64>,
	pub end_group: Option<u64>,
	/// Track timescale negotiated by the publisher. `None` means the publisher
	/// hasn't negotiated a timescale (Lite04 and earlier, or Lite05+ with the
	/// wire field set to `0`). Carried on the wire for [`Version::Lite05Wip`] and
	/// later: `None` encodes as `0`, `Some(n)` encodes as `n`.
	pub timescale: Option<Timescale>,
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
			_ => {
				self.priority.encode(w, version)?;
				(self.ordered as u8).encode(w, version)?;
				self.max_latency.encode(w, version)?;
				self.start_group.encode(w, version)?;
				self.end_group.encode(w, version)?;
				self.timescale.map(u64::from).unwrap_or(0).encode(w, version)?;
			}
		}

		Ok(())
	}

	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 => Ok(Self {
				priority: u8::decode(r, version)?,
				..Self::default()
			}),
			Version::Lite02 => Ok(Self::default()),
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
					timescale: None,
				})
			}
			_ => {
				let priority = u8::decode(r, version)?;
				let ordered = u8::decode(r, version)? != 0;
				let max_latency = std::time::Duration::decode(r, version)?;
				let start_group = Option::<u64>::decode(r, version)?;
				let end_group = Option::<u64>::decode(r, version)?;
				let timescale = Timescale::new(u64::decode(r, version)?).ok();

				Ok(Self {
					priority,
					ordered,
					max_latency,
					start_group,
					end_group,
					timescale,
				})
			}
		}
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
			_ => match self {
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
		}

		Ok(())
	}
}

impl Decode<Version> for SubscribeResponse {
	fn decode<B: bytes::Buf>(buf: &mut B, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
			_ => {
				let typ = u64::decode(buf, version)?;
				match typ {
					0 => Ok(Self::Ok(SubscribeOk::decode(buf, version)?)),
					1 => Ok(Self::Drop(SubscribeDrop::decode(buf, version)?)),
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

	fn roundtrip_ok(version: Version, original: SubscribeOk) -> SubscribeOk {
		let mut buf = BytesMut::new();
		original.encode_msg(&mut buf, version).unwrap();
		let mut bytes = buf.freeze();
		SubscribeOk::decode_msg(&mut bytes, version).unwrap()
	}

	#[test]
	fn subscribe_ok_lite04_drops_timescale() {
		// On Lite04, timescale is not serialized; it should round-trip as None.
		let ok = SubscribeOk {
			priority: 7,
			ordered: true,
			max_latency: std::time::Duration::from_millis(500),
			start_group: Some(2),
			end_group: Some(10),
			timescale: Some(Timescale::MICRO),
		};
		let decoded = roundtrip_ok(Version::Lite04, ok);
		assert_eq!(decoded.priority, 7);
		assert!(decoded.ordered);
		assert_eq!(decoded.start_group, Some(2));
		assert_eq!(decoded.end_group, Some(10));
		assert_eq!(decoded.timescale, None);
	}

	#[test]
	fn subscribe_ok_lite05_carries_timescale() {
		let ok = SubscribeOk {
			priority: 3,
			ordered: false,
			max_latency: std::time::Duration::from_millis(100),
			start_group: None,
			end_group: None,
			timescale: Some(Timescale::MICRO),
		};
		let decoded = roundtrip_ok(Version::Lite05Wip, ok);
		assert_eq!(decoded.priority, 3);
		assert_eq!(decoded.timescale, Some(Timescale::MICRO));
	}

	#[test]
	fn subscribe_ok_lite05_unspecified_timescale() {
		// timescale = None round-trips on Lite05 (wire field is 0).
		let ok = SubscribeOk::default();
		let decoded = roundtrip_ok(Version::Lite05Wip, ok);
		assert_eq!(decoded.timescale, None);
	}
}
