use bytes::{Bytes, BytesMut};
use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{Origin, OriginList, Path, coding::*, flate};

use super::{Message, Version};

/// Sent by the publisher to announce the availability of a track.
/// The payload contains the contents of the wildcard.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Announce<'a> {
	Active {
		#[cfg_attr(feature = "serde", serde(borrow))]
		suffix: Path<'a>,
		hops: OriginList,
	},
	Ended {
		#[cfg_attr(feature = "serde", serde(borrow))]
		suffix: Path<'a>,
		hops: OriginList,
	},
}

impl Message for Announce<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let status = AnnounceStatus::decode(r, version)?;
		let suffix = Path::decode(r, version)?;
		let hops = match version {
			Version::Lite01 | Version::Lite02 => OriginList::new(),
			Version::Lite03 => {
				// Lite03 sends only a hop count, not individual ids. Fill with UNKNOWN placeholders.
				// push() enforces MAX_HOPS and `?` lifts the overflow to DecodeError::BoundsExceeded.
				let count = u64::decode(r, version)? as usize;
				let mut list = OriginList::new();
				for _ in 0..count {
					list.push(Origin::UNKNOWN)?;
				}
				list
			}
			_ => OriginList::decode(r, version)?,
		};

		Ok(match status {
			AnnounceStatus::Active => Self::Active { suffix, hops },
			AnnounceStatus::Ended => Self::Ended { suffix, hops },
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match self {
			Self::Active { suffix, hops } => {
				AnnounceStatus::Active.encode(w, version)?;
				suffix.encode(w, version)?;
				encode_hops(w, version, hops)?;
			}
			Self::Ended { suffix, hops } => {
				AnnounceStatus::Ended.encode(w, version)?;
				suffix.encode(w, version)?;
				encode_hops(w, version, hops)?;
			}
		}

		Ok(())
	}
}

pub(super) struct AnnounceEncoder(flate::Encoder);

impl AnnounceEncoder {
	pub(super) fn new() -> Self {
		Self(flate::Encoder::new())
	}

	pub(super) fn encode(&mut self, announce: &Announce<'_>, version: Version) -> Result<Bytes, EncodeError> {
		let mut payload = BytesMut::new();
		announce.encode_msg(&mut payload, version)?;
		Ok(self.0.frame(&payload))
	}
}

pub(super) struct AnnounceDecoder(flate::Decoder);

impl AnnounceDecoder {
	pub(super) fn new() -> Self {
		Self(flate::Decoder::new())
	}

	pub(super) fn decode(&mut self, compressed: &[u8], version: Version) -> Result<Announce<'static>, DecodeError> {
		let payload = self.0.frame(compressed).map_err(|err| match err {
			flate::Error::TooLarge(_) => DecodeError::BoundsExceeded,
			flate::Error::Decompress => DecodeError::InvalidValue,
			_ => DecodeError::InvalidValue,
		})?;

		let mut payload = &payload[..];
		let announce = Announce::decode_msg(&mut payload, version)?;
		if !payload.is_empty() {
			return Err(DecodeError::Long);
		}

		Ok(announce)
	}
}

fn encode_hops<W: bytes::BufMut>(w: &mut W, version: Version, hops: &OriginList) -> Result<(), EncodeError> {
	match version {
		Version::Lite01 | Version::Lite02 => Ok(()),
		Version::Lite03 => (hops.len() as u64).encode(w, version),
		_ => hops.encode(w, version),
	}
}

/// Sent by the subscriber to request ANNOUNCE messages.
#[derive(Clone, Debug)]
pub struct AnnounceInterest<'a> {
	// Request tracks with this prefix.
	pub prefix: Path<'a>,
	// If non-zero, the publisher SHOULD skip announces whose hop IDs contain this value.
	pub exclude_hop: u64,
}

impl Message for AnnounceInterest<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let prefix = Path::decode(r, version)?;
		let exclude_hop = match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => 0,
			_ => u64::decode(r, version)?,
		};
		Ok(Self { prefix, exclude_hop })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.prefix.encode(w, version)?;
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {}
			_ => {
				self.exclude_hop.encode(w, version)?;
			}
		}

		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn compressed_announce_round_trips() {
		let mut hops = OriginList::new();
		hops.push(Origin::random()).unwrap();
		hops.push(Origin::random()).unwrap();

		let msg = Announce::Active {
			suffix: Path::new("room/alice/video"),
			hops,
		};

		let mut encoder = AnnounceEncoder::new();
		let compressed = encoder.encode(&msg, Version::Lite05Wip).unwrap();
		let mut decoder = AnnounceDecoder::new();
		let decoded = decoder.decode(&compressed, Version::Lite05Wip).unwrap();

		assert_eq!(decoded, msg);
	}

	#[test]
	fn compressed_announce_reuses_stream_context() {
		let mut hops = OriginList::new();
		hops.push(Origin::random()).unwrap();

		let first = Announce::Active {
			suffix: Path::new("conference/room-a/participant-a/video"),
			hops: hops.clone(),
		};
		let second = Announce::Active {
			suffix: Path::new("conference/room-a/participant-b/video"),
			hops,
		};

		let mut encoder = AnnounceEncoder::new();
		let first = encoder.encode(&first, Version::Lite05Wip).unwrap();
		let second = encoder.encode(&second, Version::Lite05Wip).unwrap();

		assert!(
			second.len() < first.len(),
			"second announce {} should reuse context from first {}",
			second.len(),
			first.len()
		);
	}
}

/// Send by the publisher, used to determine the message that follows.
#[derive(Clone, Copy, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum AnnounceStatus {
	Ended = 0,
	Active = 1,
}

impl Decode<Version> for AnnounceStatus {
	fn decode<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let status = u8::decode(r, version)?;
		status.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl Encode<Version> for AnnounceStatus {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		(*self as u8).encode(w, version)
	}
}

/// Sent after setup to communicate the initially announced paths.
///
/// Used by Draft01/Draft02 only. Draft03 uses individual Announce messages instead.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct AnnounceInit<'a> {
	/// List of currently active broadcasts, encoded as suffixes to be combined with the prefix.
	#[cfg_attr(feature = "serde", serde(borrow))]
	pub suffixes: Vec<Path<'a>>,
}

impl Message for AnnounceInit<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				return Err(DecodeError::Version);
			}
		}

		let count = u64::decode(r, version)?;

		// Don't allocate more than 1024 elements upfront
		let mut paths = Vec::with_capacity(count.min(1024) as usize);

		for _ in 0..count {
			paths.push(Path::decode(r, version)?);
		}

		Ok(Self { suffixes: paths })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		match version {
			Version::Lite01 | Version::Lite02 => {}
			_ => {
				return Err(EncodeError::Version);
			}
		}

		(self.suffixes.len() as u64).encode(w, version)?;
		for path in &self.suffixes {
			path.encode(w, version)?;
		}

		Ok(())
	}
}
