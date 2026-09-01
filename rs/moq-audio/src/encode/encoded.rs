//! [`Encoded`]: one encoded packet on its way to a track.

use bytes::Bytes;

use crate::Activity;

/// One encoded audio packet, plus what the codec was doing when it produced it.
///
/// Named for what it holds rather than mirroring [`Frame`](crate::Frame), so the
/// two never read alike at a call site that handles both.
#[derive(Clone, Debug)]
pub struct Encoded {
	/// The packet, in the framing the matching catalog importer expects.
	pub payload: Bytes,
	/// Real audio, or the comfort noise Opus sends while the input is silent.
	/// Always [`Activity::Active`] for codecs without a DTX mode.
	pub activity: Activity,
}

impl Encoded {
	/// A packet carrying real audio.
	pub fn new(payload: Bytes) -> Self {
		Self {
			payload,
			activity: Activity::Active,
		}
	}
}
