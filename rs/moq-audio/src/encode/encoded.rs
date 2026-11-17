//! [`Encoded`]: one encoded packet on its way to a track.

use bytes::Bytes;

use crate::Activity;

/// One encoded audio packet, plus what the codec was doing when it produced it.
///
/// Named for what it holds rather than mirroring [`Frame`](crate::Frame), so the
/// two never read alike at a call site that handles both.
///
/// `#[non_exhaustive]`: construct packets with [`Encoded::new`] so the record
/// can gain metadata without breaking callers.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Encoded {
	/// The packet, in the framing the matching catalog importer expects.
	pub payload: Bytes,
	/// Whether this packet codes audio, or withholds it because the input was
	/// silent. Always [`Activity::Active`] for codecs without a discontinuous
	/// mode, and for the coded frames that punctuate a silent run.
	pub activity: Activity,
	/// Whether the codec was in discontinuous transmission once it coded this
	/// packet. A coded packet in a silent run is a keep-alive, not speech, so a
	/// voice producer drops it.
	pub(crate) in_dtx: bool,
}

impl Encoded {
	/// A packet carrying real audio.
	pub fn new(payload: Bytes) -> Self {
		Self {
			payload,
			activity: Activity::Active,
			in_dtx: false,
		}
	}
}
