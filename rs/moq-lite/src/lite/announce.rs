use num_enum::{IntoPrimitive, TryFromPrimitive};

use crate::{
	Path,
	coding::*,
	lite::{Message, Version},
};

/// Sent by the publisher to announce the availability of a track.
/// The payload contains the contents of the wildcard.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Announce<'a> {
	Active {
		#[cfg_attr(feature = "serde", serde(borrow))]
		suffix: Path<'a>,
		hops: u64,
	},
	Ended {
		#[cfg_attr(feature = "serde", serde(borrow))]
		suffix: Path<'a>,
		hops: u64,
	},
}

impl Message for Announce<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let status = AnnounceStatus::decode(r, version)?;
		let suffix = Path::decode(r, version)?;
		let hops = match version {
			Version::Draft03 => u64::decode(r, version)?,
			Version::Draft01 | Version::Draft02 => 0,
		};

		Ok(match status {
			AnnounceStatus::Active => Self::Active { suffix, hops },
			AnnounceStatus::Ended => Self::Ended { suffix, hops },
		})
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match self {
			Self::Active { suffix, hops } => {
				AnnounceStatus::Active.encode(w, version);
				suffix.encode(w, version);
				match version {
					Version::Draft03 => hops.encode(w, version),
					Version::Draft01 | Version::Draft02 => {}
				}
			}
			Self::Ended { suffix, hops } => {
				AnnounceStatus::Ended.encode(w, version);
				suffix.encode(w, version);
				match version {
					Version::Draft03 => hops.encode(w, version),
					Version::Draft01 | Version::Draft02 => {}
				}
			}
		}
	}
}

/// Sent by the subscriber to request ANNOUNCE messages.
#[derive(Clone, Debug)]
pub struct AnnouncePlease<'a> {
	// Request tracks with this prefix.
	pub prefix: Path<'a>,
}

impl Message for AnnouncePlease<'_> {
	fn decode_msg<R: bytes::Buf>(r: &mut R, version: Version) -> Result<Self, DecodeError> {
		let prefix = Path::decode(r, version)?;
		Ok(Self { prefix })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		self.prefix.encode(w, version)
	}
}

/// Send by the publisher, used to determine the message that follows.
#[derive(Clone, Copy, Debug, IntoPrimitive, TryFromPrimitive)]
#[repr(u8)]
enum AnnounceStatus {
	Ended = 0,
	Active = 1,
}

impl<V> Decode<V> for AnnounceStatus {
	fn decode<R: bytes::Buf>(r: &mut R, version: V) -> Result<Self, DecodeError> {
		let status = u8::decode(r, version)?;
		status.try_into().map_err(|_| DecodeError::InvalidValue)
	}
}

impl<V> Encode<V> for AnnounceStatus {
	fn encode<W: bytes::BufMut>(&self, w: &mut W, version: V) {
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
			Version::Draft01 | Version::Draft02 => {}
			Version::Draft03 => unreachable!("announce init not supported for version: {:?}", version),
		}

		let count = u64::decode(r, version)?;

		// Don't allocate more than 1024 elements upfront
		let mut paths = Vec::with_capacity(count.min(1024) as usize);

		for _ in 0..count {
			paths.push(Path::decode(r, version)?);
		}

		Ok(Self { suffixes: paths })
	}

	fn encode_msg<W: bytes::BufMut>(&self, w: &mut W, version: Version) {
		match version {
			Version::Draft01 | Version::Draft02 => {}
			Version::Draft03 => unreachable!("announce init not supported for version: {:?}", version),
		}

		(self.suffixes.len() as u64).encode(w, version);
		for path in &self.suffixes {
			path.encode(w, version);
		}
	}
}
