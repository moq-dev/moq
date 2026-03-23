use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{OriginId, Path, coding::*};

use super::{Message, Version};

// Helper: decode an OriginId from a varint (u64) using the lite Version.
fn decode_origin_id<R: bytes::Buf>(r: &mut R, version: Version) -> Result<OriginId, DecodeError> {
	let value = u64::decode(r, version)?;
	Ok(OriginId::decode_raw(value))
}

// Helper: encode an OriginId as a varint (u64) using the lite Version.
fn encode_origin_id<W: bytes::BufMut>(id: &OriginId, w: &mut W, version: Version) -> Result<(), EncodeError> {
	id.into_inner().encode(w, version)
}

/// Sent by the publisher to announce the availability of a track.
/// The payload contains the contents of the wildcard.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Announce<'a> {
	Active {
		#[cfg_attr(feature = "serde", serde(borrow))]
		suffix: Path<'a>,
		hops: Vec<OriginId>,
	},
	Ended {
		#[cfg_attr(feature = "serde", serde(borrow))]
		suffix: Path<'a>,
		hops: Vec<OriginId>,
	},
}

impl Message for Announce<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let status = AnnounceStatus::decode(r, version)?;
		let suffix = Path::decode(r, version)?;
		let hops = match version {
			Version::Lite01 | Version::Lite02 => Vec::new(),
			Version::Lite03 => {
				// Lite03 sends a single varint count; decode but we don't know the actual IDs.
				let _count = u64::decode(r, version)?;
				Vec::new()
			}
			_ => {
				// Lite04+: count followed by that many OriginId varints.
				let count = u64::decode(r, version)?;
				let mut ids = Vec::with_capacity(count.min(32) as usize);
				for _ in 0..count {
					ids.push(decode_origin_id(r, version)?);
				}
				ids
			}
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
				match version {
					Version::Lite01 | Version::Lite02 => {}
					Version::Lite03 => {
						(hops.len() as u64).encode(w, version)?;
					}
					_ => {
						(hops.len() as u64).encode(w, version)?;
						for id in hops {
							encode_origin_id(id, w, version)?;
						}
					}
				}
			}
			Self::Ended { suffix, hops } => {
				AnnounceStatus::Ended.encode(w, version)?;
				suffix.encode(w, version)?;
				match version {
					Version::Lite01 | Version::Lite02 => {}
					Version::Lite03 => {
						(hops.len() as u64).encode(w, version)?;
					}
					_ => {
						(hops.len() as u64).encode(w, version)?;
						for id in hops {
							encode_origin_id(id, w, version)?;
						}
					}
				}
			}
		}

		Ok(())
	}
}

/// Sent by the subscriber to request ANNOUNCE messages.
#[derive(Clone, Debug)]
pub struct AnnouncePlease<'a> {
	// Request tracks with this prefix.
	pub prefix: Path<'a>,

	/// If set, skip announces whose hops list contains this origin ID.
	/// Used to avoid loops in the relay cluster.
	pub without_origin: Option<OriginId>,
}

impl Message for AnnouncePlease<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let prefix = Path::decode(r, version)?;
		let without_origin = match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => None,
			_ => {
				let value = u64::decode(r, version)?;
				if value == 0 { None } else { Some(OriginId::decode_raw(value)) }
			}
		};
		Ok(Self { prefix, without_origin })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) -> Result<(), EncodeError> {
		self.prefix.encode(w, version)?;
		match version {
			Version::Lite01 | Version::Lite02 | Version::Lite03 => {}
			_ => {
				let value = self.without_origin.map_or(0u64, |id| id.into_inner());
				value.encode(w, version)?;
			}
		}

		Ok(())
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
